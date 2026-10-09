//! The schedule against a fake clock, a scripted update source and a gate
//! a test opens and closes.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use chrono::TimeZone as _;
use steno_host::fakes::{FakeClock, FakePreferences};

use super::*;
use crate::platform::FilePreferences;

fn launch_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 9, 8, 0, 0).unwrap()
}

/// Answers the checks a test queued (none left: up to date) and counts
/// every call; an install records whether the gate's hold was alive.
#[derive(Default)]
struct FakeSource {
    answers: Mutex<VecDeque<Result<Option<String>, String>>>,
    checks: AtomicUsize,
    downloads: AtomicUsize,
    download_fails: AtomicBool,
    installs: AtomicUsize,
    install_fails: AtomicBool,
    announced: Mutex<Vec<String>>,
    gate: Mutex<Option<Arc<FakeGate>>>,
    held_while_installing: Mutex<Vec<bool>>,
}

impl FakeSource {
    fn answer(&self, answer: Result<Option<&str>, &str>) {
        lock(&self.answers).push_back(
            answer
                .map(|found| found.map(str::to_owned))
                .map_err(str::to_owned),
        );
    }

    fn announced(&self) -> Vec<String> {
        lock(&self.announced).clone()
    }
}

#[async_trait]
impl UpdateSource for FakeSource {
    async fn check(&self) -> Result<Option<String>, String> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        lock(&self.answers).pop_front().unwrap_or(Ok(None))
    }

    async fn download(&self) -> Result<(), String> {
        self.downloads.fetch_add(1, Ordering::SeqCst);
        if self.download_fails.load(Ordering::SeqCst) {
            return Err("the download broke off".into());
        }
        Ok(())
    }

    async fn install_and_relaunch(&self) -> Result<(), String> {
        self.installs.fetch_add(1, Ordering::SeqCst);
        let held = lock(&self.gate)
            .as_ref()
            .is_some_and(|gate| gate.alive.load(Ordering::SeqCst) > 0);
        lock(&self.held_while_installing).push(held);
        if self.install_fails.load(Ordering::SeqCst) {
            return Err("the bundle could not be replaced".into());
        }
        Ok(())
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
    changes: Arc<AtomicUsize>,
}

impl World {
    fn new() -> Self {
        let source = Arc::new(FakeSource::default());
        let gate = Arc::new(FakeGate::default());
        *lock(&source.gate) = Some(gate.clone());
        World {
            directory: tempfile::tempdir().unwrap(),
            clock: Arc::new(FakeClock::new(launch_time())),
            preferences: Arc::new(FakePreferences::default()),
            source,
            gate,
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
            support_directory: self.directory.path().to_owned(),
            managed,
            runtime: tokio::runtime::Handle::current(),
        });
        let changes = self.changes.clone();
        *lock(&schedule.on_change) = Some(Arc::new(move || {
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
/// off it is announced once, not at every later check of the same version,
/// and nothing is downloaded or installed.
#[tokio::test]
async fn a_found_update_is_announced_once_per_version() {
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
    assert_eq!(world.source.downloads.load(Ordering::SeqCst), 0);
    assert_eq!(world.installs(), 0);
}

/// With automatic downloads on, a found update is downloaded, and it is
/// never installed while the gate says busy: the user is told instead, and
/// later ticks try the install again without a second download. Once the
/// app is idle the next tick installs while it holds the gate.
#[tokio::test]
async fn a_download_never_installs_while_the_gate_says_busy() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule();
    schedule.tick().await;
    assert_eq!(world.source.downloads.load(Ordering::SeqCst), 1);
    assert_eq!(world.installs(), 0);
    assert_eq!(world.source.announced(), ["0.12.0"]);

    for _ in 0..5 {
        world.advance(TimeDelta::hours(1));
        schedule.tick().await;
    }
    assert_eq!(world.installs(), 0);
    assert_eq!(world.source.downloads.load(Ordering::SeqCst), 1);
    assert_eq!(world.checks(), 1);

    world.gate.idle.store(true, Ordering::SeqCst);
    world.advance(TimeDelta::hours(1));
    schedule.tick().await;
    assert_eq!(world.installs(), 1);
    assert_eq!(*lock(&world.source.held_while_installing), [true]);
    assert_eq!(world.gate.alive.load(Ordering::SeqCst), 0, "released after");
    assert_eq!(world.source.announced(), ["0.12.0"]);
}

/// Idle at the check: the download installs at once, unannounced.
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

/// The stand-in gate never gives a hold, so the schedule never installs.
#[tokio::test]
async fn the_stand_in_gate_never_installs() {
    let world = World::new();
    world.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, true);
    world.source.answer(Ok(Some("0.12.0")));
    let schedule = world.schedule_with(world.preferences.clone(), Arc::new(NeverIdle), false);
    for _ in 0..30 {
        schedule.tick().await;
        world.advance(TimeDelta::hours(1));
    }
    assert_eq!(world.installs(), 0);
    assert_eq!(world.source.announced(), ["0.12.0"]);
}

/// A failed install is shown and not retried; a failed download leaves the
/// update to the announcement.
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
    assert_eq!(schedule.check_now().await, Ok(None));
    assert_eq!(world.checks(), 1);
    assert_eq!(schedule.last_check_at(), Some(launch_time()));
}

/// A package manager's install: no tick checks and Settings cannot check.
#[tokio::test]
async fn a_managed_install_never_checks() {
    let world = World::new();
    let schedule = world.schedule_with(world.preferences.clone(), world.gate.clone(), true);
    schedule.tick().await;
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
    struct Slow(tokio::sync::Notify, AtomicBool);
    #[async_trait]
    impl UpdateSource for Slow {
        async fn check(&self) -> Result<Option<String>, String> {
            self.1.store(true, Ordering::SeqCst);
            self.0.notified().await;
            Ok(None)
        }
        async fn download(&self) -> Result<(), String> {
            Ok(())
        }
        async fn install_and_relaunch(&self) -> Result<(), String> {
            Ok(())
        }
        fn announce(&self, _version: &str) {}
    }
    let world = World::new();
    let source = Arc::new(Slow(tokio::sync::Notify::new(), AtomicBool::new(false)));
    let schedule = UpdateSchedule::new(ScheduleParts {
        source: source.clone(),
        preferences: world.preferences.clone(),
        clock: world.clock.clone(),
        gate: Arc::new(NeverIdle),
        support_directory: world.directory.path().to_owned(),
        managed: false,
        runtime: tokio::runtime::Handle::current(),
    });
    let running = tokio::spawn({
        let schedule = schedule.clone();
        async move { schedule.check_now().await }
    });
    while !source.1.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
    assert!(!schedule.can_check_for_updates());
    source.0.notify_one();
    assert_eq!(running.await.unwrap(), Ok(None));
    assert!(schedule.can_check_for_updates());
}
