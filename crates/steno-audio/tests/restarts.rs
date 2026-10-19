//! A capture's restarts as one streak, over a device the test drives by
//! hand on a manual clock: how often they try, what they log, when the
//! warning comes and goes, and that nothing is lost. The tests read the
//! binary's log, so they run one at a time.

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
    CaptureBackend, CaptureConfiguration, CaptureError, CaptureInput, CaptureMode, CaptureNotice,
    CaptureResult, CaptureSession, CaptureStream, DeviceChangeReason,
};
use steno_audio::realtime::LaneFrameSink;
use steno_audio::testing::{HANG_GUARD, ManualClock};
use steno_audio::writer::CafFile;
use steno_audio::{Clock, SAMPLE_RATE};
use steno_core::AudioLane;
use steno_core::paths::file_url_path;
use tempfile::TempDir;
use uuid::Uuid;

/// Frames in one pushed callback: 10 ms at 48 kHz.
const CALLBACK: usize = 480;
/// One sample of the watch thread, the step the clock moves by.
const STEP: Duration = CaptureSession::STALL_CHECK_INTERVAL;
/// How long a stream must deliver before the warning ends (the session's
/// `STALLED_AGAIN`).
const STALLED_AGAIN: Duration = Duration::from_secs(10);

/// Stands in for a watched device on the Mac: every start hands the sink
/// over and delivers nothing until the test pushes a callback (`push`). It
/// logs at each start and stop as the live backends do, quietly inside the
/// session's quiet restarts.
struct Pushed {
    clock: Arc<ManualClock>,
    /// The sink and the lane count while started, `None` once stopped,
    /// under one lock with `push`, so no frame lands after `stop`.
    running: Mutex<Option<(Arc<LaneFrameSink>, usize)>>,
    starts: Mutex<Vec<Duration>>,
    accepted: Mutex<usize>,
    /// Callbacks pushed while stopped: audio that played and that no
    /// capture took.
    dropped: Mutex<usize>,
}

impl Pushed {
    fn new(clock: Arc<ManualClock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            running: Mutex::new(None),
            starts: Mutex::new(Vec::new()),
            accepted: Mutex::new(0),
            dropped: Mutex::new(0),
        })
    }

    /// One callback on every lane, while started; counted as dropped while
    /// stopped.
    fn push(&self) {
        let running = self.running.lock().unwrap();
        match running.as_ref() {
            Some((sink, lanes)) => {
                if sink.begin_callback(CALLBACK) {
                    for lane in 0..*lanes {
                        sink.write_slice(lane, &[0.25; CALLBACK]);
                    }
                    sink.end_callback();
                    *self.accepted.lock().unwrap() += CALLBACK;
                }
            }
            None => *self.dropped.lock().unwrap() += 1,
        }
    }

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

    fn is_running(&self) -> bool {
        self.running.lock().unwrap().is_some()
    }

    fn dropped(&self) -> usize {
        *self.dropped.lock().unwrap()
    }
}

/// A line the device logs, at `info`, or at `debug` inside the session's
/// quiet restarts.
fn device_line(what: &str) {
    if steno_audio::testing::start_log_is_quiet() {
        tracing::debug!("the device {what}");
    } else {
        tracing::info!("the device {what}");
    }
}

impl CaptureBackend for Pushed {
    fn start(
        &self,
        lanes: &[AudioLane],
        input: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        device_line("started");
        self.starts.lock().unwrap().push(self.clock.now());
        *self.running.lock().unwrap() = Some((sink, lanes.len()));
        Ok(CaptureStream {
            input: Some(CaptureInput {
                uid: input.unwrap_or("built-in").to_owned(),
                name: None,
                is_fallback: false,
            }),
            ..CaptureStream::SYNTHETIC
        })
    }

    fn stop(&self) {
        if self.running.lock().unwrap().take().is_some() {
            device_line("stopped");
        }
    }

    fn delivers_continuously(&self, _: &[AudioLane]) -> bool {
        true
    }

    fn stall_may_be_the_microphone(&self, lanes: &[AudioLane]) -> bool {
        !lanes.contains(&AudioLane::System)
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

    /// The capture's lines at `level` so far that contain `text`.
    fn count(&self, level: &str, text: &str) -> usize {
        String::from_utf8_lossy(&self.0.lock().unwrap())
            .lines()
            .filter(|line| line.trim_start().starts_with(level) && line.contains(text))
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
/// clock, with 160 s of relay, so a full relay never holds the rebuild.
fn record(mode: CaptureMode) -> (TempDir, CaptureSession, Drive) {
    record_on(mode, None)
}

/// [`record`] on the microphone `input` chose in Settings.
fn record_on(mode: CaptureMode, input: Option<&str>) -> (TempDir, CaptureSession, Drive) {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Pushed::new(clock.clone());
    let mut configuration = CaptureConfiguration::new(mode, directory.path());
    configuration.echo_cancellation = false;
    configuration.input_device_uid = input.map(str::to_owned);
    let session =
        CaptureSession::with_backend(configuration, backend.clone(), None, 16_000, clock.clone())
            .unwrap();
    session.start(Uuid::new_v4()).unwrap();
    let drive = Drive::new(clock, backend, &session);
    (directory, session, drive)
}

/// The test's side of a recording: moves the clock a `STEP` at a time once
/// every thread that sleeps on it does, so every step is the same on any
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
    /// while one runs.
    fn settle(&mut self) {
        let deadline = Instant::now() + HANG_GUARD;
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
    /// `delivering`.
    fn run(&mut self, steps: usize, delivering: bool) {
        for _ in 0..steps {
            self.settle();
            if delivering {
                let deadline = Instant::now() + HANG_GUARD;
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

/// The device's `info` lines before its restarts go quiet: the
/// recording's start, and the stop and start of the streak's first
/// restart.
const LOUD_DEVICE_LINES: usize = 4;

/// Every line the capture logged over a recording that has run `until`
/// comes about once a minute.
fn assert_about_once_a_minute(log: &Log, until: Duration) {
    let warns = log.count("WARN", "");
    let infos = log.count("INFO", "");
    let minutes = until.as_secs() / 60;
    assert!(
        warns >= 2 && warns <= 2 + minutes as usize,
        "{warns} warn lines in {until:?}"
    );
    assert!(
        infos <= LOUD_DEVICE_LINES,
        "{infos} info lines in {until:?}"
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

/// A Mac call capture whose silent output did not start delivers nothing
/// until another app plays: it is restarted on the backoff with about one
/// log line a minute and one `StillRestarting`. The restart after playback
/// begins delivers, the silence before it fills, and the warning ends once
/// it has delivered for 10 s.
#[test]
fn a_call_capture_with_nothing_playing_restarts_on_the_backoff_and_records_from_the_next_start() {
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

    while drive.backend.is_running() {
        drive.run(1, false);
    }
    let resumed_at = drive.run_until(steps_in(Duration::from_secs(10)), true, is_resumed);
    let lost = STEP * drive.backend.dropped() as u32;
    assert!(
        lost > Duration::ZERO && lost <= CaptureSession::RESTART_BACKOFF_LONGEST,
        "{lost:?} of playback before the next start"
    );
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
