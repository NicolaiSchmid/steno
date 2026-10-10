//! The schedule against a fake clock, a scripted update source and
//! dialogs, a gate over a recorder a test puts in any state and the
//! processing a test starts and ends; then the app's gate over a recorder
//! and a pipeline.

use std::collections::VecDeque;
use std::future::Future as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::Poll;

use chrono::TimeZone as _;
use steno_bridge::CaptureMode;
use steno_host::fakes::{FakeClock, FakePermissions, FakePreferences, FakeRecorder};
use steno_host::services::{INSTALLING_UPDATE, RecorderStatus};
use uuid::Uuid;

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
/// download for `download_release` while `download_stalls` is, and an
/// install for `install_release` while `install_stalls` is; a download
/// with `busy_after_download` set starts a processing job, and one with
/// `record_during_download` set starts a recording through the recorder
/// (the user's Record meanwhile). The install and the relaunch each try a
/// Record too and record whether it was refused (one that records is
/// stopped at once), and whether the gate's hold was alive at the install.
/// The installer is `installer`'s, in place when unset.
#[derive(Default)]
struct FakeSource {
    installer: Mutex<Option<Installer>>,
    /// What `tell_relaunch_waits` was told: the version and what it waits
    /// for.
    relaunch_waits: Mutex<Vec<String>>,
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
    install_stalls: AtomicBool,
    install_release: tokio::sync::Notify,
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

    /// A Record now, as a probe: whether it was refused for the install. A
    /// recording it starts is stopped at once, so the app stays idle; one
    /// under way is left alone.
    fn record_refused_now(&self) -> bool {
        let recorder = self.recorder();
        if recorder.status().state != RecordingState::Idle {
            return false;
        }
        let refused = self.try_record();
        if !refused {
            recorder.stop();
        }
        refused
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
            gate.processing.store(true, Ordering::SeqCst);
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
        if self.install_stalls.load(Ordering::SeqCst) {
            self.install_release.notified().await;
        }
        lock(&self.installed).push(version.to_owned());
        let held = lock(&self.gate)
            .as_ref()
            .is_some_and(|gate| gate.alive.load(Ordering::SeqCst) > 0);
        lock(&self.held_while_installing).push(held);
        let refused = self.record_refused_now();
        lock(&self.record_refused).push(refused);
        if self.install_fails.load(Ordering::SeqCst) {
            return Err("the bundle could not be replaced".into());
        }
        Ok(())
    }

    async fn relaunch(&self) {
        lock(&self.steps).push("relaunch");
        self.relaunches.fetch_add(1, Ordering::SeqCst);
        let refused = self.record_refused_now();
        lock(&self.record_refused).push(refused);
    }

    async fn ask(&self, question: Question<'_>) -> bool {
        lock(&self.asked).push(match question {
            Question::Install(version) => format!("install {version}"),
            Question::AfterItEnds(Busy::Recording) => "after the recording".to_owned(),
            Question::AfterItEnds(Busy::Processing) => "after the processing".to_owned(),
        });
        lock(&self.replies).pop_front().unwrap_or(false)
    }

    fn installer(&self) -> Installer {
        lock(&self.installer).unwrap_or(Installer::InPlace)
    }

    fn tell_install_failed(&self, message: &str) {
        lock(&self.told).push(message.to_owned());
    }

    fn tell_relaunch_waits(&self, version: &str, busy: Busy) {
        lock(&self.relaunch_waits).push(format!("{version} after the {busy:?}"));
    }

    fn announce(&self, version: &str) {
        lock(&self.announced).push(version.to_owned());
    }
}

/// [`IdleGate`] over the world's recorder and the processing a test
/// starts and ends (`processing`): idle while neither runs; a hold holds
/// recording starts off, one hold at a time. Counts the holds alive.
struct FakeGate {
    processing: AtomicBool,
    recorder: Arc<FakeRecorder>,
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
        if !self.is_idle_now() || self.alive.load(Ordering::SeqCst) > 0 {
            return None;
        }
        let starts = self.recorder.hold_starts();
        self.alive.fetch_add(1, Ordering::SeqCst);
        Some(InstallHold::new(starts, Hold(self.alive.clone())))
    }

    fn is_idle_now(&self) -> bool {
        !self.processing.load(Ordering::SeqCst)
            && self.recorder.status().state == RecordingState::Idle
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
        let clock = Arc::new(FakeClock::new(launch_time()));
        let recorder = Arc::new(FakeRecorder::new(
            clock.clone(),
            Arc::new(FakePermissions::all_granted()),
        ));
        *lock(&source.recorder) = Some(recorder.clone());
        let gate = Arc::new(FakeGate {
            processing: AtomicBool::new(false),
            recorder: recorder.clone(),
            alive: Arc::default(),
        });
        *lock(&source.gate) = Some(gate.clone());
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

    /// A processing job starts (true) or ends (false).
    fn processing(&self, runs: bool) {
        self.gate.processing.store(runs, Ordering::SeqCst);
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

/// With automatic downloads on, nothing downloads while the app is busy,
/// since a recording or a processing job has the disk and the network to
/// itself: the user is told instead, and the first tick after the app is
/// idle downloads and installs, without a second check.
#[tokio::test]
async fn nothing_downloads_while_the_app_is_busy() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.processing(true);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    for _ in 0..5 {
        schedule.tick().await;
        world.advance(TimeDelta::hours(1));
    }
    assert_eq!(world.checks(), 1);
    assert_eq!(world.downloads(), 0);
    assert_eq!(world.source.announced(), ["0.12.0"]);

    world.processing(false);
    world.recording(RecordingState::Recording);
    schedule.tick().await;
    assert_eq!(world.downloads(), 0, "nor while a recording runs");

    world.recording(RecordingState::Idle);
    schedule.tick().await;
    assert_eq!(world.downloads(), 1);
    assert_eq!(world.installs(), 1);
    assert_eq!(world.checks(), 1);
}

/// A processing job that starts during the download: the download never
/// installs while it runs, and later ticks try the install again without
/// a second download. Once the app is idle the next tick installs the kept
/// package while it holds the gate, recording starts held off.
#[tokio::test]
async fn a_download_never_installs_while_the_gate_says_busy() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
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

    world.processing(false);
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.installs(), 1);
    assert_eq!(world.downloads(), 1);
    assert_eq!(*lock(&world.source.held_while_installing), [true]);
    assert_eq!(*lock(&world.source.installed), ["0.12.0"]);
    assert_eq!(*lock(&world.source.record_refused), [false, true]);
    assert_eq!(world.relaunches(), 1);
    assert_eq!(world.gate.alive.load(Ordering::SeqCst), 0, "released after");
    assert_eq!(world.source.announced(), ["0.12.0"]);
}

/// Idle at the check: the download installs at once, unannounced.
#[tokio::test]
async fn an_idle_app_installs_the_download_at_once() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(world.installs(), 1);
    assert_eq!(*lock(&world.source.held_while_installing), [true]);
    assert_eq!(world.source.announced(), Vec::<String>::new());
}

/// Automatic downloads turned off while the download runs: the flag is
/// read again when it ends, so the same tick keeps and installs nothing,
/// and the found update is announced instead.
#[tokio::test(start_paused = true)]
async fn a_switch_turned_off_during_the_download_keeps_and_installs_nothing() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.source.download_stalls.store(true, Ordering::SeqCst);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    let tick = tokio::spawn({
        let schedule = schedule.clone();
        async move { schedule.tick().await }
    });
    while world.downloads() == 0 {
        tokio::task::yield_now().await;
    }
    schedule.set_automatically_downloads(false);
    world.source.download_release.notify_one();
    at_once(tick).await.unwrap();
    assert!(schedule.state().staged.is_none());
    assert_eq!(world.installs(), 0);
    assert_eq!(world.source.announced(), ["0.12.0"]);
}

/// Turning automatic downloads off frees the kept package, so an idle
/// tick afterwards installs nothing.
#[tokio::test]
async fn turning_automatic_downloads_off_frees_the_download() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
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
    world.processing(false);
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.installs(), 0);
}

/// A check that finds no update forgets the download kept for the old
/// one, so an idle tick afterwards does not install it.
#[tokio::test]
async fn a_check_that_finds_no_update_forgets_the_download() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
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

    world.processing(false);
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.installs(), 0);
}

/// A check that finds another version forgets the package kept for the
/// old one and downloads the new one; one that finds the kept version
/// again downloads nothing.
#[tokio::test]
async fn a_check_that_finds_another_update_replaces_the_download() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
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

    world.processing(false);
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

/// Busy at the check: a later check that finds no update forgets the
/// version still to download, so the first idle tick downloads nothing.
#[tokio::test]
async fn a_check_that_finds_no_update_forgets_the_version_to_download() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.processing(true);
    world.source.answer(Ok(Some("0.12.0")));
    world.source.answer(Ok(None));
    let schedule = world.schedule();
    schedule.tick().await;
    world.advance(CHECK_INTERVAL);
    schedule.tick().await;
    assert_eq!(world.checks(), 2);
    assert_eq!(schedule.state().to_download, None);

    world.processing(false);
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.downloads(), 0);
}

/// A yes while idle installs at once, without the second question: the
/// download, then the install with processing jobs held off, then the
/// relaunch with recording starts held off too. A Record at the install
/// records; one at the relaunch is refused.
#[tokio::test]
async fn a_yes_while_idle_downloads_then_installs_and_holds_recording_off_to_relaunch() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    world.source.reply(&[true]);
    schedule.offer("0.12.0").await;
    assert_eq!(world.source.asked(), ["install 0.12.0"]);
    assert_eq!(world.source.steps(), ["download", "install", "relaunch"]);
    assert_eq!(*lock(&world.source.held_while_installing), [true]);
    assert_eq!(*lock(&world.source.record_refused), [false, true]);
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
/// yes asks whether to install once it ends, and after "Not Now" nothing
/// downloads and the first idle tick announces the version again, the
/// user's check included.
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
        ["install 0.12.0", "after the recording"]
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

/// Spawns the user's yes to `version` on the runtime.
fn offer_in_background(
    schedule: &Arc<UpdateSchedule>,
    version: &'static str,
) -> tokio::task::JoinHandle<()> {
    let schedule = schedule.clone();
    tokio::spawn(async move { schedule.offer(version).await })
}

/// A yes while a recording runs, in every state it can be in (starting,
/// recording, stopping while it saves), and then "Install After It Ends":
/// nothing downloads and nothing stops the recording; once it ends the
/// update downloads, installs, and relaunches with recording starts held
/// off.
#[tokio::test(start_paused = true)]
async fn a_yes_while_recording_waits_for_it_to_end_then_installs() {
    for state in [
        RecordingState::Starting,
        RecordingState::Recording,
        RecordingState::Stopping,
    ] {
        let world = World::new();
        world.source.answer(Ok(Some("0.12.0")));
        let schedule = world.schedule();
        schedule.tick().await;
        world.recording(state);
        world.source.reply(&[true, true]);
        let offer = offer_in_background(&schedule, "0.12.0");
        tokio::time::sleep(IDLE_POLL * 30).await;
        assert_eq!(
            world.source.asked(),
            ["install 0.12.0", "after the recording"],
            "{state:?}"
        );
        assert_eq!(world.source.steps(), Vec::<&str>::new(), "{state:?}");
        assert_eq!(world.recorder.status().state, state, "never stopped");
        assert_eq!(*lock(&world.recorder.stops), 0, "{state:?}");

        world.recording(RecordingState::Idle);
        at_once(offer).await.unwrap();
        assert_eq!(world.source.steps(), ["download", "install", "relaunch"]);
        assert_eq!(*lock(&world.source.record_refused), [false, true]);
        assert_eq!(world.recorder.status().error, None, "cleared with the hold");
    }
}

/// A yes while a meeting is processed asks whether to install once that
/// is done, and waits: nothing downloads until the processing ended.
#[tokio::test(start_paused = true)]
async fn a_yes_while_processing_waits_for_it_to_end_then_installs() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    world.processing(true);
    world.source.reply(&[true, true]);
    let offer = offer_in_background(&schedule, "0.12.0");
    tokio::time::sleep(IDLE_POLL * 30).await;
    assert_eq!(
        world.source.asked(),
        ["install 0.12.0", "after the processing"]
    );
    assert_eq!(world.downloads(), 0);

    world.processing(false);
    at_once(offer).await.unwrap();
    assert_eq!(world.source.steps(), ["download", "install", "relaunch"]);
}

/// A yes given while idle, and a Record during the download: the
/// recording is never stopped by a click given before it existed. The
/// install waits for it to end, holding the package, and then installs
/// without a second download or a second question.
#[tokio::test(start_paused = true)]
async fn a_recording_started_during_the_download_is_waited_for() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    world
        .source
        .record_during_download
        .store(true, Ordering::SeqCst);
    world.source.reply(&[true]);
    let offer = offer_in_background(&schedule, "0.12.0");
    tokio::time::sleep(IDLE_POLL * 30).await;
    assert_eq!(world.source.steps(), ["download"]);
    assert_eq!(world.recorder.status().state, RecordingState::Recording);
    assert_eq!(world.relaunches(), 0);

    world.recording(RecordingState::Idle);
    at_once(offer).await.unwrap();
    assert_eq!(world.source.steps(), ["download", "install", "relaunch"]);
    assert_eq!(world.downloads(), 1);
    assert_eq!(world.source.asked(), ["install 0.12.0"]);
}

/// A check during the wait for the hold that finds a newer version: the
/// yes keeps for it, so the install downloads the newer one, without the
/// hold, and installs it, with no error.
#[tokio::test(start_paused = true)]
async fn a_newer_version_found_during_the_wait_keeps_the_yes() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    world.source.answer(Ok(Some("0.13.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    world.source.download_stalls.store(true, Ordering::SeqCst);
    world.source.reply(&[true]);
    let offer = offer_in_background(&schedule, "0.12.0");
    while world.downloads() == 0 {
        tokio::task::yield_now().await;
    }
    assert_eq!(schedule.check_on_request().await, Ok(Some("0.13.0".into())));
    world.recording(RecordingState::Recording);
    world.source.download_stalls.store(false, Ordering::SeqCst);
    world.source.download_release.notify_one();
    tokio::time::sleep(IDLE_POLL * 30).await;
    world.recording(RecordingState::Idle);
    at_once(offer).await.unwrap();
    assert_eq!(world.downloads(), 2);
    assert_eq!(*lock(&world.source.installed), ["0.13.0"]);
    assert_eq!(world.relaunches(), 1);
    assert_eq!(*lock(&world.source.told), Vec::<String>::new());
}

/// A check during the wait for an idle app that finds a newer version:
/// the yes keeps for it, and the download is of the newer one.
#[tokio::test(start_paused = true)]
async fn a_newer_version_found_before_the_download_is_downloaded_instead() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    world.source.answer(Ok(Some("0.13.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    world.processing(true);
    world.source.reply(&[true, true]);
    let offer = offer_in_background(&schedule, "0.12.0");
    tokio::time::sleep(IDLE_POLL * 3).await;
    assert_eq!(schedule.check_on_request().await, Ok(Some("0.13.0".into())));
    world.processing(false);
    at_once(offer).await.unwrap();
    assert_eq!(world.downloads(), 1);
    assert_eq!(*lock(&world.source.installed), ["0.13.0"]);
    assert_eq!(*lock(&world.source.told), Vec::<String>::new());
}

/// While the user's install downloads, a tick downloads nothing and
/// installs nothing, and takes no hold: the user's install runs, once.
#[tokio::test(start_paused = true)]
async fn a_tick_during_the_users_install_downloads_and_installs_nothing() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.processing(true);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(schedule.state().to_download.as_deref(), Some("0.12.0"));
    world.processing(false);
    world.source.download_stalls.store(true, Ordering::SeqCst);
    world.source.reply(&[true]);
    let offer = offer_in_background(&schedule, "0.12.0");
    while world.downloads() == 0 {
        tokio::task::yield_now().await;
    }

    world.source.download_stalls.store(false, Ordering::SeqCst);
    world.advance(TimeDelta::hours(1));
    at_once(schedule.tick()).await;
    assert_eq!(world.downloads(), 1, "the user's download only");
    assert_eq!(world.installs(), 0, "the user's install runs");
    assert_eq!(world.gate.alive.load(Ordering::SeqCst), 0, "no hold taken");

    world.source.download_release.notify_one();
    at_once(offer).await.unwrap();
    assert_eq!(world.installs(), 1);
}

/// The schedule's install holds processing jobs off while the updater
/// waits: a yes given meanwhile neither waits for that hold nor installs a
/// second time, and the schedule's install goes on.
#[tokio::test(start_paused = true)]
async fn a_yes_during_the_schedules_install_does_not_wait_for_its_hold() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.source.answer(Ok(Some("0.12.0")));
    world.source.install_stalls.store(true, Ordering::SeqCst);
    let schedule = world.schedule();
    let tick = tokio::spawn({
        let schedule = schedule.clone();
        async move { schedule.tick().await }
    });
    while world.installs() == 0 {
        tokio::task::yield_now().await;
    }
    assert_eq!(world.gate.alive.load(Ordering::SeqCst), 1);
    world.source.reply(&[true]);
    at_once(schedule.offer("0.12.0")).await;
    assert_eq!(world.installs(), 1);

    world.source.install_release.notify_one();
    at_once(tick).await.unwrap();
    assert_eq!(world.installs(), 1);
    assert_eq!(world.relaunches(), 1);
    assert_eq!(world.gate.alive.load(Ordering::SeqCst), 0);
}

/// A hold taken elsewhere, as the schedule's install keeps one: the
/// user's install, once downloaded, waits for it and installs once it is
/// dropped.
#[tokio::test(start_paused = true)]
async fn the_users_install_waits_for_a_hold_held_elsewhere_then_installs() {
    let world = World::new();
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    let held = world.gate.try_hold().expect("idle");
    world.source.reply(&[true]);
    let offer = offer_in_background(&schedule, "0.12.0");
    tokio::time::sleep(IDLE_POLL * 30).await;
    assert_eq!(world.source.steps(), ["download"]);

    drop(held);
    at_once(offer).await.unwrap();
    assert_eq!(world.source.steps(), ["download", "install", "relaunch"]);
}

/// Two dialogs answered yes: while the first one's install waits, the
/// second installs nothing and downloads nothing, whatever its answers;
/// the first installs once the recording started meanwhile ends.
#[tokio::test(start_paused = true)]
async fn a_second_yes_during_an_install_does_nothing() {
    for second_dialog in [[true, false], [true, true]] {
        let world = World::new();
        world.source.answer(Ok(Some("0.12.0")));
        let schedule = world.schedule();
        schedule.tick().await;
        world.source.download_stalls.store(true, Ordering::SeqCst);
        world.source.reply(&[true]);
        let first = offer_in_background(&schedule, "0.12.0");
        while world.downloads() == 0 {
            tokio::task::yield_now().await;
        }
        world.recording(RecordingState::Recording);
        world.source.reply(&second_dialog);
        at_once(schedule.offer("0.12.0")).await;
        assert_eq!(world.downloads(), 1, "{second_dialog:?}");

        world.source.download_release.notify_one();
        tokio::time::sleep(IDLE_POLL * 30).await;
        assert_eq!(world.source.steps(), ["download"], "{second_dialog:?}");
        world.recording(RecordingState::Idle);
        at_once(first).await.unwrap();
        assert_eq!(
            world.source.steps(),
            ["download", "install", "relaunch"],
            "{second_dialog:?}"
        );
    }
}

/// The user's install takes the package kept for the version on offer,
/// without a second download. A failed install is shown and told, and a
/// Record after the failed install records.
#[tokio::test]
async fn the_users_install_takes_the_package_kept_for_its_version() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world
        .source
        .busy_after_download
        .store(true, Ordering::SeqCst);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(world.installs(), 0);
    world
        .source
        .busy_after_download
        .store(false, Ordering::SeqCst);
    world.processing(false);

    assert!(schedule.state().staged.is_some());
    assert_eq!(
        schedule.install_on_request("0.12.0").await,
        Installed::Relaunching
    );
    assert!(schedule.state().staged.is_none());
    assert_eq!(*lock(&world.source.installed), ["0.12.0"]);
    assert_eq!(world.downloads(), 1);

    world.source.install_fails.store(true, Ordering::SeqCst);
    assert_eq!(
        schedule.install_on_request("0.12.0").await,
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

/// Idle: a failed install is shown and not retried; a failed download
/// leaves the update to the announcement.
#[tokio::test]
async fn failed_downloads_and_installs_fall_back_to_the_announcement() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
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
    let schedule = world.schedule_with(preferences.clone(), world.gate.clone(), false);
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
        world.gate.clone(),
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

/// The app's gate over a fake recorder and a pipeline over core's fakes,
/// with a ready meeting a test can claim a re-export of.
struct AppGate {
    _directory: tempfile::TempDir,
    recorder: Arc<FakeRecorder>,
    pipeline: Arc<CurrentPipeline>,
    meeting: Uuid,
    gate: IdleGate,
}

impl AppGate {
    fn new() -> Self {
        let (directory, store) = crate::testing::temp_store();
        let mut meeting = steno_core::testing::sample_data::meeting();
        meeting.state = steno_core::MeetingState::Ready;
        store.save_meeting(&meeting).unwrap();
        let pipeline =
            crate::testing::current_pipeline(crate::testing::fake_dependencies(&store, "fake"));
        let recorder = Arc::new(FakeRecorder::new(
            Arc::new(FakeClock::new(launch_time())),
            Arc::new(FakePermissions::all_granted()),
        ));
        let gate = IdleGate::new(recorder.clone(), pipeline.clone());
        AppGate {
            _directory: directory,
            recorder,
            pipeline,
            meeting: meeting.id,
            gate,
        }
    }

    /// A re-export of the ready meeting, claimed now: a job in flight
    /// until it ends or is dropped.
    fn claim_a_job(&self) -> steno_pipeline::Operation {
        self.pipeline
            .current()
            .claim_redeliver(self.meeting)
            .unwrap()
    }
}

/// No hold while a recording starts, runs or stops (its save runs while
/// it stops), and taking none leaves Record free: no start hold stays
/// behind and no refusal shows. An idle recorder gives one.
#[tokio::test]
async fn the_apps_gate_gives_no_hold_while_a_recording_is_under_way() {
    let app = AppGate::new();
    for state in [
        RecordingState::Starting,
        RecordingState::Recording,
        RecordingState::Stopping,
    ] {
        app.recorder.set_status(RecorderStatus {
            state,
            ..RecorderStatus::idle()
        });
        assert!(!app.gate.is_idle_now(), "{state:?}");
        assert!(app.gate.try_hold().is_none(), "{state:?}");
        assert_eq!(app.recorder.status().error, None, "{state:?}");
    }
    app.recorder.set_status(RecorderStatus::idle());
    assert!(app.gate.is_idle_now());
    assert!(app.gate.try_hold().is_some());
}

/// No hold while a processing job is in flight, from its claim to its
/// end, nor once the app shuts down; one hold at a time.
#[tokio::test]
async fn the_apps_gate_gives_no_hold_while_processing_or_shutting_down() {
    let app = AppGate::new();
    let job = app.claim_a_job();
    assert!(!app.gate.is_idle_now());
    assert!(app.gate.try_hold().is_none());
    drop(job);
    assert!(app.gate.is_idle_now());

    let hold = app.gate.try_hold().expect("idle");
    assert!(app.gate.try_hold().is_none(), "one hold at a time");
    drop(hold);

    app.pipeline.quit();
    assert!(!app.gate.is_idle_now());
    assert!(app.gate.try_hold().is_none());
}

/// The gate's hold refuses a recording start, with the message, until it
/// is dropped; then a start records.
#[tokio::test]
async fn the_apps_gate_hold_refuses_a_start() {
    let app = AppGate::new();
    let hold = app.gate.try_hold().expect("idle");
    app.recorder.start(CaptureMode::InPerson, None);
    let status = app.recorder.status();
    assert_eq!(status.state, RecordingState::Idle);
    assert_eq!(status.error.as_deref(), Some(INSTALLING_UPDATE));

    drop(hold);
    assert_eq!(app.recorder.status().error, None);
    app.recorder.start(CaptureMode::InPerson, None);
    assert_eq!(app.recorder.status().state, RecordingState::Recording);
}

/// A processing job claimed while the gate's hold lives, as one for a
/// phone recording that arrives during the install: it keeps its claim and
/// waits, and runs once the hold is dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_apps_gate_hold_keeps_a_queued_job_waiting() {
    let app = AppGate::new();
    let hold = app.gate.try_hold().expect("idle");
    let mut job = app.claim_a_job();
    let waited = tokio::time::timeout(std::time::Duration::from_millis(300), &mut job).await;
    assert!(waited.is_err(), "the job waits for the hold");
    assert!(!app.gate.is_idle_now());

    drop(hold);
    tokio::time::timeout(crate::testing::PATIENCE, job)
        .await
        .expect("the job ran once the hold was dropped")
        .unwrap();
    assert!(app.gate.is_idle_now());
}

/// An installer that never returns, as a password prompt nobody answers:
/// a Record during it records at once, processing jobs are held off for
/// [`INSTALL_HOLD_LIMIT`] only, and nothing relaunches or stops the
/// recording, however long it waits.
#[tokio::test(start_paused = true)]
async fn an_installer_that_never_returns_never_keeps_recording_off() {
    for installer in [Installer::InPlace, Installer::AsksForAPassword] {
        let world = World::new();
        *lock(&world.source.installer) = Some(installer);
        world.source.install_stalls.store(true, Ordering::SeqCst);
        world.source.answer(Ok(Some("0.12.0")));
        let schedule = world.schedule();
        schedule.tick().await;
        world.source.reply(&[true]);
        let offer = offer_in_background(&schedule, "0.12.0");
        while world.installs() == 0 {
            tokio::task::yield_now().await;
        }
        assert!(!world.source.try_record(), "{installer:?}");
        assert_eq!(world.recorder.status().state, RecordingState::Recording);
        assert_eq!(world.recorder.status().error, None);
        assert_eq!(world.gate.alive.load(Ordering::SeqCst), 1, "jobs wait");

        tokio::time::sleep(INSTALL_HOLD_LIMIT + IDLE_POLL).await;
        assert_eq!(world.gate.alive.load(Ordering::SeqCst), 0, "{installer:?}");
        tokio::time::sleep(TICK * 24).await;
        assert_eq!(world.relaunches(), 0);
        assert_eq!(world.recorder.status().state, RecordingState::Recording);
        assert_eq!(*lock(&world.recorder.stops), 0);
        offer.abort();
    }
}

/// An installer that returns after a Record started a recording: the
/// relaunch waits for the recording to end, the user told so once, then
/// takes the hold and relaunches with a Record refused.
#[tokio::test(start_paused = true)]
async fn an_install_that_ends_during_a_recording_relaunches_once_it_ends() {
    let world = World::new();
    world.source.install_stalls.store(true, Ordering::SeqCst);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    world.source.reply(&[true]);
    let offer = offer_in_background(&schedule, "0.12.0");
    while world.installs() == 0 {
        tokio::task::yield_now().await;
    }
    assert!(!world.source.try_record());
    world.source.install_release.notify_one();
    tokio::time::sleep(IDLE_POLL * 30).await;
    assert_eq!(*lock(&world.source.installed), ["0.12.0"]);
    assert_eq!(world.relaunches(), 0);
    assert_eq!(world.recorder.status().state, RecordingState::Recording);
    assert_eq!(
        *lock(&world.source.relaunch_waits),
        ["0.12.0 after the Recording"]
    );

    world.recording(RecordingState::Idle);
    at_once(offer).await.unwrap();
    assert_eq!(world.relaunches(), 1);
    assert_eq!(*lock(&world.source.record_refused), [false, true]);
    assert_eq!(lock(&world.source.relaunch_waits).len(), 1, "told once");
    assert_eq!(world.gate.alive.load(Ordering::SeqCst), 0);
}

/// Where the installer ends the app (Windows), the hold is kept through
/// the install as well: a Record at the install and at the relaunch is
/// refused.
#[tokio::test]
async fn an_installer_that_ends_the_app_keeps_recording_off_through_the_install() {
    let world = World::new();
    *lock(&world.source.installer) = Some(Installer::EndsTheApp);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    world.source.reply(&[true]);
    schedule.offer("0.12.0").await;
    assert_eq!(world.source.steps(), ["download", "install", "relaunch"]);
    assert_eq!(*lock(&world.source.held_while_installing), [true]);
    assert_eq!(*lock(&world.source.record_refused), [true, true]);
    assert_eq!(world.recorder.status().error, None, "cleared with the hold");
}

/// With automatic downloads on, an installer that asks for a password is
/// not run by the schedule, since nobody may be there to answer: the kept
/// package is announced, and the user's yes installs it without a second
/// download.
#[tokio::test]
async fn the_schedule_announces_a_package_whose_install_asks_for_a_password() {
    let world = World::new();
    *lock(&world.source.installer) = Some(Installer::AsksForAPassword);
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(world.downloads(), 1);
    assert_eq!(world.installs(), 0);
    assert_eq!(world.gate.alive.load(Ordering::SeqCst), 0, "no hold taken");
    assert_eq!(world.source.announced(), ["0.12.0"]);

    world.source.reply(&[true]);
    schedule.offer("0.12.0").await;
    assert_eq!(world.source.steps(), ["download", "install", "relaunch"]);
}

/// A recorder whose `hold_starts` first runs `before`, as a start or a
/// quit landing between the gate's first look and its start hold, and
/// counts the start holds taken.
struct RacingRecorder {
    inner: Arc<FakeRecorder>,
    before: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    holds: AtomicUsize,
}

impl Recorder for RacingRecorder {
    fn status(&self) -> RecorderStatus {
        self.inner.status()
    }

    fn start(&self, mode: CaptureMode, call_app: Option<&str>) {
        self.inner.start(mode, call_app);
    }

    fn hold_starts(&self) -> StartHold {
        let before = lock(&self.before).take();
        if let Some(before) = before {
            before();
        }
        self.holds.fetch_add(1, Ordering::SeqCst);
        self.inner.hold_starts()
    }

    fn stop(&self) {
        self.inner.stop();
    }

    fn toggle(&self) {
        self.inner.toggle();
    }

    fn keep_recording(&self) {
        self.inner.keep_recording();
    }

    fn clear_messages(&self) {
        self.inner.clear_messages();
    }

    fn refresh_permissions(&self) {
        self.inner.refresh_permissions();
    }

    fn remember_audio_folder(&self, folder: &Path) {
        self.inner.remember_audio_folder(folder);
    }

    fn left_recording(&self, meeting_id: Uuid) -> steno_host::services::LeftRecording {
        self.inner.left_recording(meeting_id)
    }

    fn forget_recording(&self, meeting_id: Uuid) -> std::io::Result<Option<PathBuf>> {
        self.inner.forget_recording(meeting_id)
    }

    fn restore_recording(&self, meeting_id: Uuid, folder: &Path) {
        self.inner.restore_recording(meeting_id, folder);
    }
}

impl AppGate {
    /// The app's gate over a [`RacingRecorder`] around this one's recorder.
    fn racing(&self) -> (Arc<RacingRecorder>, IdleGate) {
        let racing = Arc::new(RacingRecorder {
            inner: self.recorder.clone(),
            before: Mutex::new(None),
            holds: AtomicUsize::new(0),
        });
        let gate = IdleGate::new(racing.clone(), self.pipeline.clone());
        (racing, gate)
    }
}

/// A Record that starts between the gate's first look and its start hold
/// gets no hold over it: the gate reads the status again under the start
/// hold, and the recording goes on with no refusal shown.
#[tokio::test]
async fn a_start_between_the_gates_look_and_its_start_hold_gets_no_hold() {
    let app = AppGate::new();
    let (racing, gate) = app.racing();
    let recorder = app.recorder.clone();
    *lock(&racing.before) = Some(Box::new(move || {
        recorder.start(CaptureMode::InPerson, None);
    }));
    assert!(gate.try_hold().is_none(), "a recording started");
    assert_eq!(app.recorder.status().state, RecordingState::Recording);
    assert_eq!(app.recorder.status().error, None);
}

/// A shutdown that begins between the gate's first look and its start
/// hold gets no hold over it either.
#[tokio::test]
async fn a_quit_between_the_gates_look_and_its_start_hold_gets_no_hold() {
    let app = AppGate::new();
    let (racing, gate) = app.racing();
    let pipeline = app.pipeline.clone();
    *lock(&racing.before) = Some(Box::new(move || pipeline.quit()));
    assert!(gate.try_hold().is_none(), "the app quits");
}

/// A busy app is told apart before the start hold, so a Record while a
/// job runs is never refused for an install that is not coming.
#[tokio::test]
async fn a_busy_app_takes_no_start_hold() {
    let app = AppGate::new();
    let (racing, gate) = app.racing();
    let job = app.claim_a_job();
    assert!(gate.try_hold().is_none());
    assert_eq!(racing.holds.load(Ordering::SeqCst), 0, "no start hold");
    drop(job);
    assert!(gate.try_hold().is_some());
    assert_eq!(racing.holds.load(Ordering::SeqCst), 1);
}

/// A tick with no package kept takes no start hold, so a Record at that
/// moment is not refused.
#[tokio::test]
async fn a_tick_with_nothing_kept_takes_no_start_hold() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    let app = AppGate::new();
    let (racing, gate) = app.racing();
    let schedule = world.schedule_with(world.preferences.clone(), Arc::new(gate), false);
    schedule.tick().await;
    assert_eq!(world.checks(), 1);
    assert_eq!(racing.holds.load(Ordering::SeqCst), 0);
}

/// Automatic downloads turned off during a download after which the app
/// is busy: the flag read when the download ends keeps nothing, so no
/// later tick installs it.
#[tokio::test(start_paused = true)]
async fn a_switch_turned_off_during_a_download_while_busy_keeps_nothing() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.source.download_stalls.store(true, Ordering::SeqCst);
    world
        .source
        .busy_after_download
        .store(true, Ordering::SeqCst);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    let tick = tokio::spawn({
        let schedule = schedule.clone();
        async move { schedule.tick().await }
    });
    while world.downloads() == 0 {
        tokio::task::yield_now().await;
    }
    schedule.set_automatically_downloads(false);
    world.source.download_release.notify_one();
    at_once(tick).await.unwrap();
    assert!(schedule.state().staged.is_none(), "not kept");
}
