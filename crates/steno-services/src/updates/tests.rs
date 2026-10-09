//! The schedule against a fake clock, a scripted update source and
//! dialogs, a gate a test opens and closes, and a recorder a test puts in
//! any state.

use std::collections::VecDeque;
use std::future::Future as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::Poll;

use chrono::TimeZone as _;
use steno_bridge::CaptureMode;
use steno_host::fakes::{FakeClock, FakePermissions, FakePreferences, FakeRecorder};
use steno_host::services::{INSTALLING_UPDATE, RecorderStatus};

use super::*;
use crate::platform::FilePreferences;

/// `future`'s output; a future that would wait for good fails the test
/// instead, on the paused clock these tests run on.
async fn at_once<F: std::future::Future>(future: F) -> F::Output {
    tokio::time::timeout(TICK, future)
        .await
        .expect("returns without waiting")
}

fn launch_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 9, 8, 0, 0).unwrap()
}

/// Answers the checks a test queued (none left: up to date) and the
/// dialogs' questions (none left: no), and records every call in order
/// (`steps`). A check waits for `release` while `stalls` is set, and a
/// download for `download_release` while `download_stalls` is; a download
/// with `busy_after_download` set leaves the gate busy, and one with
/// `record_during_download` set starts a recording through the recorder
/// (the user's Record meanwhile). The install and the relaunch each try a
/// Record too and record whether it was refused, and whether the gate's
/// hold was alive at the install.
#[derive(Default)]
struct FakeSource {
    answers: Mutex<VecDeque<Result<Option<String>, String>>>,
    checks: AtomicUsize,
    stalls: AtomicBool,
    release: tokio::sync::Notify,
    downloads: AtomicUsize,
    download_stalls: AtomicBool,
    download_release: tokio::sync::Notify,
    busy_after_download: AtomicBool,
    record_during_download: AtomicBool,
    download_fails: AtomicBool,
    installs: AtomicUsize,
    installed: Mutex<Vec<String>>,
    install_fails: AtomicBool,
    relaunches: AtomicUsize,
    replies: Mutex<VecDeque<bool>>,
    asked: Mutex<Vec<String>>,
    told: Mutex<Vec<String>>,
    announced: Mutex<Vec<String>>,
    gate: Mutex<Option<Arc<FakeGate>>>,
    recorder: Mutex<Option<Arc<FakeRecorder>>>,
    held_while_installing: Mutex<Vec<bool>>,
    /// Whether a Record at the install, then at the relaunch, was refused.
    record_refused: Mutex<Vec<bool>>,
    steps: Mutex<Vec<&'static str>>,
}

impl FakeSource {
    fn answer(&self, answer: Result<Option<&str>, &str>) {
        lock(&self.answers).push_back(
            answer
                .map(|found| found.map(str::to_owned))
                .map_err(str::to_owned),
        );
    }

    /// The user's next answers, in order.
    fn reply(&self, replies: &[bool]) {
        lock(&self.replies).extend(replies);
    }

    fn announced(&self) -> Vec<String> {
        lock(&self.announced).clone()
    }

    fn asked(&self) -> Vec<String> {
        lock(&self.asked).clone()
    }

    fn steps(&self) -> Vec<&'static str> {
        lock(&self.steps).clone()
    }

    fn recorder(&self) -> Arc<FakeRecorder> {
        lock(&self.recorder).clone().expect("the world's recorder")
    }

    /// The user's Record now: whether it was refused for the install. A
    /// recording under way ignores it, as the recorder does.
    fn try_record(&self) -> bool {
        let recorder = self.recorder();
        if recorder.status().state != RecordingState::Idle {
            return false;
        }
        recorder.start(CaptureMode::InPerson, None);
        let status = recorder.status();
        status.state == RecordingState::Idle && status.error.as_deref() == Some(INSTALLING_UPDATE)
    }
}

#[async_trait]
impl UpdateSource for FakeSource {
    async fn check(&self) -> Result<Option<String>, String> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        if self.stalls.load(Ordering::SeqCst) {
            self.release.notified().await;
        }
        lock(&self.answers).pop_front().unwrap_or(Ok(None))
    }

    async fn download(&self, _version: &str) -> Result<Vec<u8>, String> {
        lock(&self.steps).push("download");
        self.downloads.fetch_add(1, Ordering::SeqCst);
        if self.download_stalls.load(Ordering::SeqCst) {
            self.download_release.notified().await;
        }
        if self.busy_after_download.load(Ordering::SeqCst)
            && let Some(gate) = lock(&self.gate).as_ref()
        {
            gate.idle.store(false, Ordering::SeqCst);
        }
        if self.record_during_download.load(Ordering::SeqCst) {
            assert!(!self.try_record(), "a Record during the download starts");
        }
        if self.download_fails.load(Ordering::SeqCst) {
            return Err("the download broke off".into());
        }
        Ok(vec![1, 2, 3])
    }

    async fn install(&self, version: &str, package: Vec<u8>) -> Result<(), String> {
        lock(&self.steps).push("install");
        assert_eq!(package, [1, 2, 3]);
        self.installs.fetch_add(1, Ordering::SeqCst);
        lock(&self.installed).push(version.to_owned());
        let held = lock(&self.gate)
            .as_ref()
            .is_some_and(|gate| gate.alive.load(Ordering::SeqCst) > 0);
        lock(&self.held_while_installing).push(held);
        let refused = self.try_record();
        lock(&self.record_refused).push(refused);
        if self.install_fails.load(Ordering::SeqCst) {
            return Err("the bundle could not be replaced".into());
        }
        Ok(())
    }

    async fn relaunch(&self) {
        lock(&self.steps).push("relaunch");
        self.relaunches.fetch_add(1, Ordering::SeqCst);
        let refused = self.try_record();
        lock(&self.record_refused).push(refused);
    }

    async fn ask(&self, question: Question<'_>) -> bool {
        lock(&self.asked).push(match question {
            Question::Install(version) => format!("install {version}"),
            Question::StopRecording => "stop the recording".to_owned(),
        });
        lock(&self.replies).pop_front().unwrap_or(false)
    }

    fn tell_install_failed(&self, message: &str) {
        lock(&self.told).push(message.to_owned());
    }

    fn announce(&self, version: &str) {
        lock(&self.announced).push(version.to_owned());
    }
}

/// Idle when a test says so; counts the holds alive.
#[derive(Default)]
struct FakeGate {
    idle: AtomicBool,
    alive: Arc<AtomicUsize>,
}

struct Hold(Arc<AtomicUsize>);

impl Drop for Hold {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl InstallGate for FakeGate {
    fn try_hold(&self) -> Option<InstallHold> {
        if !self.idle.load(Ordering::SeqCst) {
            return None;
        }
        self.alive.fetch_add(1, Ordering::SeqCst);
        Some(Box::new(Hold(self.alive.clone())))
    }
}

struct World {
    directory: tempfile::TempDir,
    clock: Arc<FakeClock>,
    preferences: Arc<FakePreferences>,
    source: Arc<FakeSource>,
    gate: Arc<FakeGate>,
    recorder: Arc<FakeRecorder>,
    changes: Arc<AtomicUsize>,
}

impl World {
    fn new() -> Self {
        let source = Arc::new(FakeSource::default());
        let gate = Arc::new(FakeGate::default());
        *lock(&source.gate) = Some(gate.clone());
        let clock = Arc::new(FakeClock::new(launch_time()));
        let recorder = Arc::new(FakeRecorder::new(
            clock.clone(),
            Arc::new(FakePermissions::all_granted()),
        ));
        *lock(&source.recorder) = Some(recorder.clone());
        World {
            directory: tempfile::tempdir().unwrap(),
            clock,
            preferences: Arc::new(FakePreferences::default()),
            source,
            gate,
            recorder,
            changes: Arc::default(),
        }
    }

    fn file(&self) -> PathBuf {
        self.directory.path().join(LAST_CHECK_FILE)
    }

    /// A schedule over this world, as a launch builds it.
    fn schedule(&self) -> Arc<UpdateSchedule> {
        self.schedule_with(self.preferences.clone(), self.gate.clone(), false)
    }

    fn schedule_with(
        &self,
        preferences: Arc<dyn Preferences>,
        gate: Arc<dyn InstallGate>,
        managed: bool,
    ) -> Arc<UpdateSchedule> {
        let schedule = UpdateSchedule::new(ScheduleParts {
            source: self.source.clone(),
            preferences,
            clock: self.clock.clone(),
            gate,
            recorder: self.recorder.clone(),
            support_directory: self.directory.path().to_owned(),
            managed,
            runtime: tokio::runtime::Handle::current(),
        });
        let changes = self.changes.clone();
        schedule.on_change(Arc::new(move || {
            changes.fetch_add(1, Ordering::SeqCst);
        }));
        schedule
    }

    fn checks(&self) -> usize {
        self.source.checks.load(Ordering::SeqCst)
    }

    fn installs(&self) -> usize {
        self.source.installs.load(Ordering::SeqCst)
    }

    fn downloads(&self) -> usize {
        self.source.downloads.load(Ordering::SeqCst)
    }

    fn recording(&self, state: RecordingState) {
        self.recorder.set_status(RecorderStatus {
            state,
            ..RecorderStatus::idle()
        });
    }

    /// A recording of meeting `id` under way.
    fn recording_meeting(&self, id: Uuid) {
        self.recorder.set_status(RecorderStatus {
            state: RecordingState::Recording,
            meeting_id: Some(id),
            ..RecorderStatus::idle()
        });
    }

    fn relaunches(&self) -> usize {
        self.source.relaunches.load(Ordering::SeqCst)
    }

    fn advance(&self, by: TimeDelta) {
        self.clock.set(self.clock.now() + by);
    }

    fn stored_time(&self) -> String {
        std::fs::read_to_string(self.file()).unwrap()
    }
}

/// With no file, the first tick (the one at launch) checks and writes the
/// time in RFC 3339 UTC under the named key; the host hears of the start
/// and the end.
#[tokio::test]
async fn a_check_is_due_at_launch() {
    let world = World::new();
    let schedule = world.schedule();
    assert!(schedule.is_due());
    schedule.tick().await;
    assert_eq!(world.checks(), 1);
    assert_eq!(
        world.stored_time(),
        r#"{"lastCheckAt":"2026-10-09T08:00:00Z"}"#
    );
    assert_eq!(schedule.last_check_at(), Some(launch_time()));
    assert_eq!(schedule.last_outcome(), UpdateOutcome::UpToDate);
    assert_eq!(world.changes.load(Ordering::SeqCst), 2);
}

/// A check less than a day ago, read from the file at launch, skips the
/// launch tick and every hourly one until the day is over.
#[tokio::test]
async fn a_recent_check_is_skipped_until_a_day_has_passed() {
    let world = World::new();
    std::fs::write(world.file(), r#"{"lastCheckAt":"2026-10-09T06:00:00Z"}"#).unwrap();
    let schedule = world.schedule();
    assert_eq!(
        schedule.last_check_at(),
        Some(launch_time() - TimeDelta::hours(2))
    );
    for _ in 0..22 {
        schedule.tick().await;
        world.advance(TimeDelta::hours(1));
    }
    assert_eq!(world.checks(), 0, "up to 23 hours after the last check");
    schedule.tick().await;
    assert_eq!(world.checks(), 1, "a day after it");
    assert_eq!(
        world.stored_time(),
        r#"{"lastCheckAt":"2026-10-10T06:00:00Z"}"#
    );
}

/// After a check, the hourly ticks of the next day check nothing, and the
/// first tick a day later checks again.
#[tokio::test]
async fn the_hourly_tick_checks_again_a_day_later() {
    let world = World::new();
    let schedule = world.schedule();
    schedule.tick().await;
    for hour in 1..24 {
        world.advance(TimeDelta::hours(1));
        schedule.tick().await;
        assert_eq!(world.checks(), 1, "hour {hour}");
    }
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.checks(), 2);
}

/// A failed check records the failure and leaves the time alone (no file
/// at all here), so the next hourly tick checks again.
#[tokio::test]
async fn a_failed_check_is_retried_at_the_next_tick() {
    let world = World::new();
    world.source.answer(Err("offline"));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(
        schedule.last_outcome(),
        UpdateOutcome::Failed("offline".into())
    );
    assert_eq!(schedule.last_check_at(), None);
    assert!(!world.file().exists());

    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.checks(), 2);
    assert_eq!(schedule.last_outcome(), UpdateOutcome::UpToDate);
    assert_eq!(
        world.stored_time(),
        r#"{"lastCheckAt":"2026-10-09T09:00:00Z"}"#
    );
}

/// A failure after an earlier success keeps that success's time.
#[tokio::test]
async fn a_failed_check_keeps_the_last_good_time() {
    let world = World::new();
    std::fs::write(world.file(), r#"{"lastCheckAt":"2026-10-07T08:00:00Z"}"#).unwrap();
    world.source.answer(Err("offline"));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(
        world.stored_time(),
        r#"{"lastCheckAt":"2026-10-07T08:00:00Z"}"#
    );
    assert!(schedule.is_due());
}

/// A found update counts as a successful check; with automatic downloads
/// off it is announced once in a run, not at every later check of the
/// same version, and nothing is downloaded or installed.
#[tokio::test]
async fn a_found_update_is_announced_once_per_version_in_a_run() {
    let world = World::new();
    world.gate.idle.store(true, Ordering::SeqCst);
    world.source.answer(Ok(Some("0.12.0")));
    world.source.answer(Ok(Some("0.12.0")));
    world.source.answer(Ok(Some("0.12.1")));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(
        schedule.last_outcome(),
        UpdateOutcome::Available("0.12.0".into())
    );
    assert_eq!(schedule.last_check_at(), Some(launch_time()));
    for _ in 0..2 {
        world.advance(CHECK_INTERVAL);
        schedule.tick().await;
    }
    assert_eq!(world.checks(), 3);
    assert_eq!(world.source.announced(), ["0.12.0", "0.12.1"]);
    assert_eq!(world.downloads(), 0);
    assert_eq!(world.installs(), 0);
}

/// With automatic downloads on and a real gate, nothing downloads while
/// the app is busy, since a recording or a processing job has the disk and
/// the network to itself: the user is told instead, and the first tick
/// after the app is idle downloads and installs, without a second check.
#[tokio::test]
async fn nothing_downloads_while_the_app_is_busy() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    for _ in 0..5 {
        schedule.tick().await;
        world.advance(TimeDelta::hours(1));
    }
    assert_eq!(world.checks(), 1);
    assert_eq!(world.downloads(), 0);
    assert_eq!(world.source.announced(), ["0.12.0"]);

    world.gate.idle.store(true, Ordering::SeqCst);
    schedule.tick().await;
    assert_eq!(world.downloads(), 1);
    assert_eq!(world.installs(), 1);
    assert_eq!(world.checks(), 1);
}

/// With a real gate that turns busy during the download, the download
/// never installs while the gate says busy, and later ticks try the
/// install again without a second download. Once the app is idle the next
/// tick installs the kept package while it holds the gate.
#[tokio::test]
async fn a_download_never_installs_while_the_gate_says_busy() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.gate.idle.store(true, Ordering::SeqCst);
    world
        .source
        .busy_after_download
        .store(true, Ordering::SeqCst);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(world.downloads(), 1);
    assert_eq!(world.installs(), 0);
    assert_eq!(world.source.announced(), ["0.12.0"]);

    for _ in 0..5 {
        world.advance(TimeDelta::hours(1));
        schedule.tick().await;
    }
    assert_eq!(world.installs(), 0);
    assert_eq!(world.downloads(), 1);
    assert_eq!(world.checks(), 1);

    world.gate.idle.store(true, Ordering::SeqCst);
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.installs(), 1);
    assert_eq!(world.downloads(), 1);
    assert_eq!(*lock(&world.source.held_while_installing), [true]);
    assert_eq!(*lock(&world.source.installed), ["0.12.0"]);
    assert_eq!(*lock(&world.source.record_refused), [true, true]);
    assert_eq!(world.relaunches(), 1);
    assert_eq!(world.gate.alive.load(Ordering::SeqCst), 0, "released after");
    assert_eq!(world.source.announced(), ["0.12.0"]);
}

/// A real gate, idle at the check: the download installs at once,
/// unannounced.
#[tokio::test]
async fn an_idle_app_installs_the_download_at_once() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.gate.idle.store(true, Ordering::SeqCst);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(world.installs(), 1);
    assert_eq!(*lock(&world.source.held_while_installing), [true]);
    assert_eq!(world.source.announced(), Vec::<String>::new());
}

/// The stand-in gate: with automatic downloads on, nothing downloads or
/// installs by itself, and the found update is announced instead.
#[tokio::test]
async fn nothing_downloads_while_the_gate_is_the_stand_in() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule_with(world.preferences.clone(), Arc::new(NeverIdle), false);
    for _ in 0..30 {
        schedule.tick().await;
        world.advance(TimeDelta::hours(1));
    }
    assert_eq!(world.checks(), 2);
    assert_eq!(world.downloads(), 0);
    assert_eq!(world.installs(), 0);
    assert_eq!(world.source.announced(), ["0.12.0"]);
}

/// With a real gate, turning automatic downloads off frees the kept
/// package, so an idle tick afterwards installs nothing.
#[tokio::test]
async fn turning_automatic_downloads_off_frees_the_download() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.gate.idle.store(true, Ordering::SeqCst);
    world
        .source
        .busy_after_download
        .store(true, Ordering::SeqCst);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(world.downloads(), 1);
    assert!(schedule.state().staged.is_some());
    schedule.set_automatically_downloads(false);
    assert!(schedule.state().staged.is_none());

    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.gate.idle.store(true, Ordering::SeqCst);
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.installs(), 0);
}

/// With a real gate, a check that finds no update forgets the download
/// kept for the old one, so an idle tick afterwards does not install it.
#[tokio::test]
async fn a_check_that_finds_no_update_forgets_the_download() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.gate.idle.store(true, Ordering::SeqCst);
    world
        .source
        .busy_after_download
        .store(true, Ordering::SeqCst);
    world.source.answer(Ok(Some("0.12.0")));
    world.source.answer(Ok(None));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(world.downloads(), 1);
    world.advance(CHECK_INTERVAL);
    schedule.tick().await;
    assert_eq!(world.checks(), 2);

    world.gate.idle.store(true, Ordering::SeqCst);
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.installs(), 0);
}

/// With a real gate, a check that finds another version forgets the
/// package kept for the old one and downloads the new one; one that finds
/// the kept version again downloads nothing.
#[tokio::test]
async fn a_check_that_finds_another_update_replaces_the_download() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.gate.idle.store(true, Ordering::SeqCst);
    world
        .source
        .busy_after_download
        .store(true, Ordering::SeqCst);
    world.source.answer(Ok(Some("0.12.0")));
    world.source.answer(Ok(Some("0.12.0")));
    world.source.answer(Ok(Some("0.13.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(world.downloads(), 1);

    world.gate.idle.store(true, Ordering::SeqCst);
    world.advance(CHECK_INTERVAL);
    schedule.check_on_request().await.unwrap();
    assert_eq!(
        schedule.state().to_download,
        None,
        "the same version is kept"
    );

    world.advance(CHECK_INTERVAL);
    schedule.check_on_request().await.unwrap();
    assert!(schedule.state().staged.is_none());
    assert_eq!(schedule.state().to_download.as_deref(), Some("0.13.0"));
}

/// With a real gate, busy at the check: a later check that finds no
/// update forgets the version still to download, so the first idle tick
/// downloads nothing.
#[tokio::test]
async fn a_check_that_finds_no_update_forgets_the_version_to_download() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.source.answer(Ok(Some("0.12.0")));
    world.source.answer(Ok(None));
    let schedule = world.schedule();
    schedule.tick().await;
    world.advance(CHECK_INTERVAL);
    schedule.tick().await;
    assert_eq!(world.checks(), 2);
    assert_eq!(schedule.state().to_download, None);

    world.gate.idle.store(true, Ordering::SeqCst);
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.downloads(), 0);
}

/// Installing stops a recording that is starting, running or stopping,
/// and only an idle recorder, or the recording the user agreed to stop,
/// lets an install through.
#[test]
fn only_an_idle_recorder_lets_an_install_through() {
    let (agreed, other) = (Uuid::new_v4(), Uuid::new_v4());
    assert!(!stops_a_recording(RecordingState::Idle));
    assert!(may_stop(&RecorderStatus::idle(), None));
    for state in [
        RecordingState::Starting,
        RecordingState::Recording,
        RecordingState::Stopping,
    ] {
        assert!(stops_a_recording(state), "{state:?}");
        let status = |meeting_id| RecorderStatus {
            state,
            meeting_id,
            ..RecorderStatus::idle()
        };
        assert!(!may_stop(&status(Some(agreed)), None), "{state:?}");
        assert!(may_stop(&status(Some(agreed)), Some(agreed)), "{state:?}");
        assert!(!may_stop(&status(Some(other)), Some(agreed)), "{state:?}");
        assert!(!may_stop(&status(None), Some(agreed)), "{state:?}");
    }
}

/// A yes while idle installs at once, without the second question: the
/// download, then the install with recording starts held off, then the
/// relaunch, with a Record at the install and at the relaunch refused.
#[tokio::test]
async fn a_yes_while_idle_downloads_then_holds_recording_off_and_installs() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    world.source.reply(&[true]);
    schedule.offer("0.12.0").await;
    assert_eq!(world.source.asked(), ["install 0.12.0"]);
    assert_eq!(world.source.steps(), ["download", "install", "relaunch"]);
    assert_eq!(*lock(&world.source.record_refused), [true, true]);
    assert_eq!(world.recorder.status().state, RecordingState::Idle);
    assert_eq!(world.recorder.status().error, None, "cleared with the hold");
}

/// "Later" installs nothing and leaves the version announced.
#[tokio::test]
async fn later_installs_nothing() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    schedule.offer("0.12.0").await;
    assert_eq!(world.source.asked(), ["install 0.12.0"]);
    assert_eq!(world.source.steps(), Vec::<&str>::new());
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.source.announced(), ["0.12.0"]);
}

/// A dialog shown while idle and answered once a recording started: the
/// yes asks again, and after "Not Now" nothing downloads and the first
/// idle tick announces the version again, the user's check included.
#[tokio::test]
async fn a_yes_put_off_by_a_recording_is_announced_again() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(world.source.announced(), ["0.12.0"]);
    assert!(!schedule.recording_under_way());

    world.recording(RecordingState::Recording);
    assert!(schedule.recording_under_way());
    world.source.reply(&[true, false]);
    schedule.offer("0.12.0").await;
    assert_eq!(
        world.source.asked(),
        ["install 0.12.0", "stop the recording"]
    );
    assert_eq!(world.downloads(), 0);
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.source.announced(), ["0.12.0"]);

    world.recording(RecordingState::Idle);
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.source.announced(), ["0.12.0", "0.12.0"]);

    world.source.answer(Ok(Some("0.12.0")));
    assert_eq!(schedule.check_on_request().await, Ok(Some("0.12.0".into())));
    schedule.announce_again("0.12.0");
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.source.announced(), ["0.12.0", "0.12.0", "0.12.0"]);
}

/// Announcing again names one version: another version's leaves the
/// announced one alone, so the next idle tick asks nothing.
#[tokio::test]
async fn announcing_another_version_again_leaves_the_announced_one() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    schedule.announce_again("0.12.1");
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.source.announced(), ["0.12.0"]);
}

/// A yes after the recording started, confirmed: the install stops that
/// recording, and a Record meanwhile is refused.
#[tokio::test]
async fn a_confirmed_yes_installs_over_the_recording_it_named() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    world.recording_meeting(Uuid::new_v4());
    world.source.reply(&[true, true]);
    schedule.offer("0.12.0").await;
    assert_eq!(
        world.source.asked(),
        ["install 0.12.0", "stop the recording"]
    );
    assert_eq!(world.source.steps(), ["download", "install", "relaunch"]);
}

/// A yes given while idle, and a Record during the download: the install
/// is put off, so the recording is not ended by a click given before it
/// existed. The package is kept, the version is announced again at the
/// first idle tick, and the next yes installs it without a second
/// download.
#[tokio::test]
async fn a_recording_started_during_the_download_puts_the_install_off() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    world
        .source
        .record_during_download
        .store(true, Ordering::SeqCst);
    world.source.reply(&[true]);
    schedule.offer("0.12.0").await;
    assert_eq!(world.source.steps(), ["download"]);
    assert_eq!(world.recorder.status().state, RecordingState::Recording);
    assert_eq!(world.relaunches(), 0);
    assert!(schedule.state().staged.is_some());
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.source.announced(), ["0.12.0"], "not while recording");

    world.recording(RecordingState::Idle);
    world
        .source
        .record_during_download
        .store(false, Ordering::SeqCst);
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.source.announced(), ["0.12.0", "0.12.0"]);
    world.source.reply(&[true]);
    schedule.offer("0.12.0").await;
    assert_eq!(world.source.steps(), ["download", "install", "relaunch"]);
    assert_eq!(world.downloads(), 1);
}

/// A confirmed yes names the recording it may stop: when that one ended
/// during the download and another began, the install is put off.
#[tokio::test(start_paused = true)]
async fn a_confirmed_yes_does_not_stop_a_later_recording() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    world.recording_meeting(Uuid::new_v4());
    world.source.download_stalls.store(true, Ordering::SeqCst);
    world.source.reply(&[true, true]);
    let offer = tokio::spawn({
        let schedule = schedule.clone();
        async move { schedule.offer("0.12.0").await }
    });
    while world.downloads() == 0 {
        tokio::task::yield_now().await;
    }
    world.recording_meeting(Uuid::new_v4());
    world.source.download_release.notify_one();
    at_once(offer).await.unwrap();
    assert_eq!(world.source.steps(), ["download"]);
    assert!(schedule.state().staged.is_some());
}

/// A put-off install whose version a check replaced during the download
/// keeps nothing: the newer version is the one to install.
#[tokio::test(start_paused = true)]
async fn a_put_off_install_of_a_replaced_version_keeps_nothing() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    world.source.answer(Ok(Some("0.13.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    world.source.download_stalls.store(true, Ordering::SeqCst);
    world.source.reply(&[true]);
    let offer = tokio::spawn({
        let schedule = schedule.clone();
        async move { schedule.offer("0.12.0").await }
    });
    while world.downloads() == 0 {
        tokio::task::yield_now().await;
    }
    assert_eq!(schedule.check_on_request().await, Ok(Some("0.13.0".into())));
    world.recording(RecordingState::Recording);
    world.source.download_release.notify_one();
    at_once(offer).await.unwrap();
    assert_eq!(world.installs(), 0);
    assert!(schedule.state().staged.is_none());
}

/// With a real gate, idle: a tick while the user's install downloads
/// installs nothing, though it downloaded the package itself.
#[tokio::test(start_paused = true)]
async fn a_tick_during_the_users_install_installs_nothing() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(schedule.state().to_download.as_deref(), Some("0.12.0"));
    world.source.download_stalls.store(true, Ordering::SeqCst);
    world.source.reply(&[true]);
    let offer = tokio::spawn({
        let schedule = schedule.clone();
        async move { schedule.offer("0.12.0").await }
    });
    while world.downloads() == 0 {
        tokio::task::yield_now().await;
    }

    world.source.download_stalls.store(false, Ordering::SeqCst);
    world.gate.idle.store(true, Ordering::SeqCst);
    world.advance(TimeDelta::hours(1));
    at_once(schedule.tick()).await;
    assert_eq!(world.downloads(), 2);
    assert_eq!(world.installs(), 0, "the user's install runs");

    world.source.download_release.notify_one();
    at_once(offer).await.unwrap();
    assert_eq!(world.installs(), 1);
}

/// Two dialogs answered yes: while the first one's download runs, the
/// second installs nothing and downloads nothing. A recording started
/// meanwhile puts the first one's install off, whatever the second
/// dialog's answers were.
#[tokio::test(start_paused = true)]
async fn a_second_yes_during_an_install_does_nothing() {
    for second_dialog in [[true, false], [true, true]] {
        let world = World::new();
        world.source.answer(Ok(Some("0.12.0")));
        let schedule = world.schedule();
        schedule.tick().await;
        world.source.download_stalls.store(true, Ordering::SeqCst);
        world.source.reply(&[true]);
        let first = tokio::spawn({
            let schedule = schedule.clone();
            async move { schedule.offer("0.12.0").await }
        });
        while world.downloads() == 0 {
            tokio::task::yield_now().await;
        }
        world.recording_meeting(Uuid::new_v4());
        world.source.reply(&second_dialog);
        at_once(schedule.offer("0.12.0")).await;
        assert_eq!(world.downloads(), 1, "{second_dialog:?}");

        world.source.download_release.notify_one();
        first.await.unwrap();
        assert_eq!(world.source.steps(), ["download"], "{second_dialog:?}");
        assert!(schedule.state().staged.is_some(), "{second_dialog:?}");
    }
}

/// The user's install takes the package kept for the version the dialog
/// named, and only for that version: a yes for a version the last check
/// replaced installs nothing, since the newer one has its own dialog. A
/// failed install is shown and told, and a Record after the failed
/// install records.
#[tokio::test]
async fn the_users_install_takes_the_package_kept_for_its_version() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.gate.idle.store(true, Ordering::SeqCst);
    world
        .source
        .busy_after_download
        .store(true, Ordering::SeqCst);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(world.installs(), 0);

    assert_eq!(
        schedule.install_on_request("0.12.1", None).await,
        Installed::Skipped
    );
    assert!(
        schedule.state().staged.is_some(),
        "kept for its own version"
    );
    assert_eq!(
        schedule.install_on_request("0.12.0", None).await,
        Installed::Relaunching
    );
    assert!(schedule.state().staged.is_none());
    assert_eq!(*lock(&world.source.installed), ["0.12.0"]);
    assert_eq!(world.downloads(), 1);

    world.source.install_fails.store(true, Ordering::SeqCst);
    assert_eq!(
        schedule.install_on_request("0.12.0", None).await,
        Installed::Failed
    );
    let failed = "the bundle could not be replaced";
    assert_eq!(
        schedule.last_outcome(),
        UpdateOutcome::Failed(failed.into())
    );
    assert_eq!(*lock(&world.source.told), [failed]);
    assert_eq!(world.recorder.status().error, None, "the hold is gone");
    world.recorder.start(CaptureMode::InPerson, None);
    assert_eq!(world.recorder.status().state, RecordingState::Recording);
}

/// A yes for a version no check found any more fails with a message, and
/// downloads nothing.
#[tokio::test]
async fn a_yes_for_a_version_no_longer_found_is_refused() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    world.source.answer(Ok(None));
    let schedule = world.schedule();
    schedule.tick().await;
    schedule.check_on_request().await.unwrap();
    world.source.reply(&[true]);
    schedule.offer("0.12.0").await;
    assert_eq!(world.downloads(), 0);
    assert_eq!(
        *lock(&world.source.told),
        ["Steno 0.12.0 is no longer the update on offer."]
    );
}

/// While a recording starts, runs or stops, a found update is not
/// announced, since the dialog would offer to end the recording; the first
/// tick after it ends announces it, once.
#[tokio::test]
async fn a_found_update_waits_for_the_recording_to_end() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    for state in [
        RecordingState::Starting,
        RecordingState::Recording,
        RecordingState::Stopping,
    ] {
        world.recording(state);
        schedule.tick().await;
        world.advance(TimeDelta::hours(1));
    }
    assert_eq!(
        schedule.last_outcome(),
        UpdateOutcome::Available("0.12.0".into())
    );
    assert_eq!(world.source.announced(), Vec::<String>::new());

    world.recording(RecordingState::Idle);
    schedule.tick().await;
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.source.announced(), ["0.12.0"]);
    assert_eq!(world.checks(), 1);
}

/// The user's check shows its own dialog, so the schedule does not
/// announce the version it found, not even at its next day's check.
#[tokio::test]
async fn the_users_check_counts_as_the_announcement() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    assert_eq!(schedule.check_on_request().await, Ok(Some("0.12.0".into())));
    schedule.tick().await;
    world.advance(CHECK_INTERVAL);
    schedule.tick().await;
    assert_eq!(world.checks(), 2);
    assert_eq!(world.source.announced(), Vec::<String>::new());
}

/// A check that does not answer fails after [`CHECK_TIMEOUT`], on a
/// paused clock: the tick ends, Settings' button comes back and the next check
/// runs.
#[tokio::test(start_paused = true)]
async fn a_stalled_check_fails_after_a_minute_and_frees_the_next() {
    let world = World::new();
    world.source.stalls.store(true, Ordering::SeqCst);
    let schedule = world.schedule();
    let started = tokio::time::Instant::now();
    tokio::time::timeout(TICK, schedule.tick())
        .await
        .expect("the check gives up before the next tick");
    let waited = started.elapsed();
    assert!(
        waited >= CHECK_TIMEOUT && waited < CHECK_TIMEOUT * 2,
        "{waited:?}"
    );
    assert_eq!(
        schedule.last_outcome(),
        UpdateOutcome::Failed(CHECK_TIMED_OUT.into())
    );
    assert_eq!(schedule.last_check_at(), None);
    assert!(schedule.can_check_for_updates());

    world.source.stalls.store(false, Ordering::SeqCst);
    assert_eq!(schedule.check_on_request().await, Ok(None));
    assert_eq!(world.checks(), 2);
}

/// A tick that waited behind the user's check finds the check no longer
/// due and does not ask the lanes a second time.
#[tokio::test]
async fn a_tick_behind_the_users_check_does_not_check_again() {
    let world = World::new();
    world.source.stalls.store(true, Ordering::SeqCst);
    let schedule = world.schedule();
    let users = tokio::spawn({
        let schedule = schedule.clone();
        async move { schedule.check_on_request().await }
    });
    while world.checks() == 0 {
        tokio::task::yield_now().await;
    }
    let mut tick = std::pin::pin!(schedule.tick());
    let first = std::future::poll_fn(|context| Poll::Ready(tick.as_mut().poll(context))).await;
    assert!(first.is_pending(), "the tick waits for the user's check");

    world.source.release.notify_one();
    assert_eq!(users.await.unwrap(), Ok(None));
    tick.await;
    assert_eq!(world.checks(), 1);
}

/// With a real gate, idle: a failed install is shown and not retried; a
/// failed download leaves the update to the announcement.
#[tokio::test]
async fn failed_downloads_and_installs_fall_back_to_the_announcement() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.gate.idle.store(true, Ordering::SeqCst);
    world.source.install_fails.store(true, Ordering::SeqCst);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(world.installs(), 1);
    assert_eq!(
        schedule.last_outcome(),
        UpdateOutcome::Failed("the bundle could not be replaced".into())
    );
    assert_eq!(world.source.announced(), ["0.12.0"]);
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.installs(), 1, "nothing staged any more");

    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.gate.idle.store(true, Ordering::SeqCst);
    world.source.download_fails.store(true, Ordering::SeqCst);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(world.installs(), 0);
    assert_eq!(world.source.announced(), ["0.12.0"]);
}

/// Automatic checks off: no tick checks, while the user's check still
/// runs and records.
#[tokio::test]
async fn automatic_checks_off_leaves_only_the_users_check() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_CHECKS_KEY, false);
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(world.checks(), 0);
    assert_eq!(schedule.check_on_request().await, Ok(None));
    assert_eq!(world.checks(), 1);
    assert_eq!(schedule.last_check_at(), Some(launch_time()));
}

/// A package manager's install: no tick checks, Settings cannot check, and
/// the tray's check fails without a request.
#[tokio::test]
async fn a_managed_install_never_checks() {
    let world = World::new();
    let schedule = world.schedule_with(world.preferences.clone(), world.gate.clone(), true);
    schedule.tick().await;
    assert_eq!(
        schedule.check_on_request().await,
        Err(MANAGED_CHECK.to_owned())
    );
    assert_eq!(world.checks(), 0);
    assert!(!schedule.can_check_for_updates());
    assert!(world.schedule().can_check_for_updates());
}

/// The environment wins over the build's value.
#[test]
fn the_environment_decides_a_managed_install_before_the_build() {
    assert!(managed_by(Some("aur"), None));
    assert!(managed_by(None, Some("nix")));
    assert!(managed_by(Some("nix"), Some("aur")));
    assert!(!managed_by(Some("appimage"), Some("nix")));
    assert!(!managed_by(None, None));
    assert!(!managed_by(Some("NIX"), None));
}

/// A time in the future (the clock was set back) is due, so a bad stored
/// time cannot stop the checks.
#[tokio::test]
async fn a_check_time_in_the_future_is_due() {
    let world = World::new();
    std::fs::write(world.file(), r#"{"lastCheckAt":"2027-01-01T00:00:00Z"}"#).unwrap();
    assert!(world.schedule().is_due());
}

/// A time with an offset (written by hand) reads as the same instant.
#[tokio::test]
async fn a_time_with_an_offset_reads_as_utc() {
    let world = World::new();
    std::fs::write(
        world.file(),
        r#"{"lastCheckAt":"2026-10-09T09:00:00+02:00"}"#,
    )
    .unwrap();
    assert_eq!(
        world.schedule().last_check_at(),
        Some(launch_time() - TimeDelta::hours(1))
    );
}

/// A corrupt file, one without the key and one whose time does not parse
/// read as no check yet: the launch tick checks and replaces the file.
#[tokio::test]
async fn a_corrupt_file_reads_as_missing_and_is_replaced() {
    for corrupt in [
        &b"{\"lastCheckAt\": \"2026-10"[..],
        br#"{"checkedAt":"2026-10-09T07:00:00Z"}"#,
        br#"{"lastCheckAt":"yesterday"}"#,
        br#"{"lastCheckAt":1790000000}"#,
        b"\xff\xfe",
    ] {
        let world = World::new();
        std::fs::write(world.file(), corrupt).unwrap();
        let schedule = world.schedule();
        assert_eq!(schedule.last_check_at(), None);
        schedule.tick().await;
        assert_eq!(world.checks(), 1);
        assert_eq!(
            world.stored_time(),
            r#"{"lastCheckAt":"2026-10-09T08:00:00Z"}"#
        );
    }
}

/// The file is replaced, never written into: a second name for the old
/// file keeps the old bytes, and no temporary is left beside it.
#[tokio::test]
async fn the_time_is_written_atomically() {
    let world = World::new();
    let old = br#"{"lastCheckAt":"2026-10-01T08:00:00Z"}"#;
    std::fs::write(world.file(), old).unwrap();
    let second_name = world.directory.path().join("old-name");
    std::fs::hard_link(world.file(), &second_name).unwrap();
    world.schedule().tick().await;
    assert_eq!(std::fs::read(&second_name).unwrap(), old);
    assert_eq!(
        world.stored_time(),
        r#"{"lastCheckAt":"2026-10-09T08:00:00Z"}"#
    );
    let mut names: Vec<String> = std::fs::read_dir(world.directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["old-name", LAST_CHECK_FILE]);
}

/// The flags are the two named booleans in `preferences.json`; missing,
/// they read as a fresh Sparkle install (checks on, downloads off), and a
/// value that is no boolean reads as missing. Settings writes booleans,
/// which a build that reads the file as a map of booleans still parses.
#[tokio::test]
async fn the_flags_are_booleans_under_their_keys_with_sparkles_defaults() {
    assert_eq!(AUTOMATIC_CHECKS_KEY, "steno.updates.automaticChecks");
    assert_eq!(AUTOMATIC_DOWNLOAD_KEY, "steno.updates.automaticDownload");
    let world = World::new();
    let path = world.directory.path().join("preferences.json");
    std::fs::write(&path, br#"{"steno.updates.automaticDownload":"yes"}"#).unwrap();
    let preferences = Arc::new(FilePreferences::new(&path));
    let schedule = world.schedule_with(preferences.clone(), Arc::new(NeverIdle), false);
    assert!(schedule.automatically_checks());
    assert!(!schedule.automatically_downloads());

    schedule.set_automatically_checks(false);
    schedule.set_automatically_downloads(true);
    assert!(!schedule.automatically_checks());
    assert!(schedule.automatically_downloads());
    let stored: std::collections::BTreeMap<String, bool> =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        stored,
        [
            (AUTOMATIC_CHECKS_KEY.to_owned(), false),
            (AUTOMATIC_DOWNLOAD_KEY.to_owned(), true),
        ]
        .into()
    );
    let reread = world.schedule_with(
        Arc::new(FilePreferences::new(&path)),
        Arc::new(NeverIdle),
        false,
    );
    assert!(!reread.automatically_checks());
    assert!(reread.automatically_downloads());
}

/// While a check runs, Settings cannot start another.
#[tokio::test]
async fn no_second_check_starts_while_one_runs() {
    let world = World::new();
    world.source.stalls.store(true, Ordering::SeqCst);
    let schedule = world.schedule();
    let running = tokio::spawn({
        let schedule = schedule.clone();
        async move { schedule.check_on_request().await }
    });
    while world.checks() == 0 {
        tokio::task::yield_now().await;
    }
    assert!(!schedule.can_check_for_updates());
    world.source.release.notify_one();
    assert_eq!(running.await.unwrap(), Ok(None));
    assert!(schedule.can_check_for_updates());
}
