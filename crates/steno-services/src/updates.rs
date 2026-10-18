//! The update schedule: the host's `Updater` over the shell's update
//! source. It checks at launch and daily, announces a found update, and
//! never installs over a recording or a processing job: an install runs
//! only while it holds the [`InstallGate`]'s [`InstallHold`]. Packaged
//! installs ([`updates_are_managed`]) get no schedule and no check.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use steno_bridge::RecordingState;
use steno_host::services::{Clock, Preferences, Recorder, StartHold, UpdateOutcome, Updater};

use crate::files::{Access, replace_file};
use crate::pipeline::CurrentPipeline;

/// Whether the schedule checks by itself.
pub const AUTOMATIC_CHECKS_KEY: &str = "steno.updates.automaticChecks";
/// Whether a found update is downloaded and installed by itself once the
/// install gate allows it.
pub const AUTOMATIC_DOWNLOAD_KEY: &str = "steno.updates.automaticDownload";
/// [`AUTOMATIC_CHECKS_KEY`] when `preferences.json` lacks it.
pub const AUTOMATIC_CHECKS_DEFAULT: bool = true;
/// [`AUTOMATIC_DOWNLOAD_KEY`] when `preferences.json` lacks it.
pub const AUTOMATIC_DOWNLOAD_DEFAULT: bool = false;
/// The file under the support directory that holds the last check time.
pub const LAST_CHECK_FILE: &str = "update-check.json";
/// The key in [`LAST_CHECK_FILE`]: an RFC 3339 time in UTC.
pub const LAST_CHECK_KEY: &str = "lastCheckAt";
/// How old the last check may grow before the schedule checks again.
pub const CHECK_INTERVAL: TimeDelta = TimeDelta::days(1);
/// How often the schedule looks at the last check time.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(60 * 60);
/// How long a check may take before it fails, so a stalled request cannot
/// hold every later check. It wraps the check alone, not the download.
pub const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// How often the user's waiting install looks again.
pub const IDLE_POLL: std::time::Duration = std::time::Duration::from_secs(2);
/// How long an installer that returns keeps processing jobs waiting; it
/// never holds recording starts off. Past a minute nobody is answering
/// the password prompt.
pub const JOB_HOLD_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);
/// How long the user's download may take before an installer that asks an
/// administrator asks again: the user may have left the screen.
pub const LONG_DOWNLOAD: std::time::Duration = std::time::Duration::from_secs(30);
/// The outcome of a check that ran into [`CHECK_TIMEOUT`].
pub const CHECK_TIMED_OUT: &str = "The update check timed out.";
/// The outcome of a check asked for on a packaged install.
pub const MANAGED_CHECK: &str = steno_host::settings::snapshots::MANAGED_UPDATES;

/// The variable a package sets to say it delivers the updates.
pub const DISTRIBUTION_VARIABLE: &str = "STENO_DISTRIBUTION";

/// Where Nix keeps its builds, read-only to everything but Nix itself.
const NIX_STORE: &str = "/nix/store";
/// Where nix-darwin copies a package's apps.
const NIX_DARWIN_APPS: &str = "/Applications/Nix Apps";
/// Where home-manager (`targets.darwin.copyApps`) copies a package's apps
/// by default, under the home folder; a custom directory is not
/// recognised.
const HOME_MANAGER_APPS: &str = "Applications/Home Manager Apps";

/// Whether a package manager delivers this app's updates:
/// [`DISTRIBUTION_VARIABLE`] is `aur` or `nix` (the environment's value
/// before the build's), or the running binary, its links resolved, lies
/// in a folder Nix fills. Such a binary is managed whatever the variable
/// says: an update cannot replace it in the read-only store, and the next
/// switch would undo one made to a copy.
pub fn updates_are_managed() -> bool {
    managed_install(
        std::env::var(DISTRIBUTION_VARIABLE).ok().as_deref(),
        option_env!("STENO_DISTRIBUTION"),
        std::env::current_exe().ok().as_deref(),
        &nix_folders(std::env::var_os("HOME").as_deref()),
    )
}

/// [`updates_are_managed`] from its inputs.
fn managed_install(
    environment: Option<&str>,
    build: Option<&str>,
    executable: Option<&Path>,
    folders: &[PathBuf],
) -> bool {
    managed_by(environment, build)
        || executable.is_some_and(|executable| runs_from(executable, folders))
}

fn managed_by(environment: Option<&str>, build: Option<&str>) -> bool {
    matches!(environment.or(build), Some("aur" | "nix"))
}

/// The folders Nix fills: the store, nix-darwin's copies, and
/// home-manager's default folder under `home`, left out when `home` is
/// unset or not absolute.
fn nix_folders(home: Option<&OsStr>) -> Vec<PathBuf> {
    let mut folders = vec![PathBuf::from(NIX_STORE), PathBuf::from(NIX_DARWIN_APPS)];
    folders.extend(
        home.map(Path::new)
            .filter(|home| home.is_absolute())
            .map(|home| home.join(HOME_MANAGER_APPS)),
    );
    folders
}

fn runs_from(executable: &Path, folders: &[PathBuf]) -> bool {
    let executable = resolved(executable);
    folders
        .iter()
        .any(|folder| executable.starts_with(folder) || executable.starts_with(resolved(folder)))
}

/// `path` with its links resolved, or as given when it does not exist.
fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// What an install holds ([`InstallGate::try_hold`]): until it is dropped
/// no recording starts and no processing job begins. The schedule lets
/// each part go on its own, the starts first.
pub struct InstallHold {
    starts: Option<StartHold>,
    jobs: Option<Box<dyn Send>>,
}

impl InstallHold {
    #[must_use]
    pub fn new(starts: StartHold, jobs: impl Send + 'static) -> Self {
        InstallHold {
            starts: Some(starts),
            jobs: Some(Box::new(jobs)),
        }
    }
}

/// Whether an update may install and relaunch the app now; the app's is
/// [`IdleGate`].
pub trait InstallGate: Send + Sync {
    /// A hold when the app is idle, `None` while it is busy. One hold lives
    /// at a time.
    fn try_hold(&self) -> Option<InstallHold>;
    /// Whether the app is idle now, without keeping a hold.
    fn is_idle_now(&self) -> bool {
        self.try_hold().is_some()
    }
    /// Whether the app is shutting down, so a waiting relaunch never comes.
    fn is_shutting_down(&self) -> bool {
        false
    }
}

/// The app's [`InstallGate`], over the recorder and the pipelines'
/// in-flight set. Idle means no recording starts, runs or stops, the app
/// is not shutting down, and no processing job runs or waits to (a
/// meeting waiting for models does not count).
pub struct IdleGate {
    recorder: Arc<dyn Recorder>,
    pipeline: Arc<CurrentPipeline>,
}

impl IdleGate {
    #[must_use]
    pub fn new(recorder: Arc<dyn Recorder>, pipeline: Arc<CurrentPipeline>) -> Self {
        IdleGate { recorder, pipeline }
    }
}

impl InstallGate for IdleGate {
    /// Takes the recorder's start hold, then reads its status, so no
    /// recording starts after the read; then the pipelines' job hold. A busy
    /// app is told apart first without a start hold, so a Record clicked
    /// while a job runs is never refused for an install that is not coming.
    fn try_hold(&self) -> Option<InstallHold> {
        if !self.is_idle_now() {
            return None;
        }
        let starts = self.recorder.hold_starts();
        if under_way(self.recorder.status().state) || self.pipeline.quitting() {
            return None;
        }
        let jobs = self.pipeline.in_flight().try_hold()?;
        Some(InstallHold::new(starts, jobs))
    }

    fn is_idle_now(&self) -> bool {
        !under_way(self.recorder.status().state)
            && !self.pipeline.quitting()
            && self.pipeline.in_flight().is_idle()
    }

    fn is_shutting_down(&self) -> bool {
        self.pipeline.quitting()
    }
}

/// Whether a recording is starting, running or stopping.
fn under_way(state: RecordingState) -> bool {
    state != RecordingState::Idle
}

/// What the user's install asks ([`UpdateSource::ask`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Question<'a> {
    /// The update alert for this version: install it now and relaunch?
    Install(&'a str),
    /// A yes given while the app is busy: install once that ends?
    AfterItEnds(Busy),
    /// A yes for an installer that asks an administrator, put off while the
    /// app was busy or by a long download, asked again now that it is idle.
    InstallNow(&'a str),
}

/// What an install waits for, as [`Question::AfterItEnds`] names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    /// A recording starts, runs or is being saved.
    Recording,
    /// A meeting is being processed, or waits to be; or the app is
    /// shutting down.
    Processing,
}

/// How an install ended, when it returned.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Installed {
    /// The relaunch is under way (in a test, it returned).
    Relaunching,
    /// The install or its download failed, or the update is gone; the user
    /// is told.
    Failed,
    /// Another install runs, so nothing was installed.
    Skipped,
    /// The user answered no to [`Question::InstallNow`].
    PutOff,
}

/// How [`UpdateSource::install`] writes the new version, which decides
/// how long the install holds recording starts and processing jobs off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installer {
    /// Replaces the app where it lies and returns: an `AppImage`, or a macOS
    /// bundle the user can write.
    InPlace,
    /// Asks for an administrator's password, then installs and returns: a
    /// `.deb` or an `.rpm`, or a macOS bundle the user cannot write.
    AsksForAPassword,
    /// Ends the app and leaves the installer to a watcher: Windows' NSIS
    /// setup. A failed setup starts the old version again.
    EndsTheApp,
    /// Ends the app; the installer then asks for an administrator's consent:
    /// Windows' MSI. A no or a failure starts the old version again.
    EndsTheAppThenAsks,
}

impl Installer {
    /// Whether the install returns, so recording starts go on while it
    /// runs, and processing jobs after [`JOB_HOLD_LIMIT`].
    #[must_use]
    pub fn returns(self) -> bool {
        matches!(self, Installer::InPlace | Installer::AsksForAPassword)
    }

    /// Whether the install asks an administrator, so it needs someone at the
    /// screen.
    #[must_use]
    pub fn asks_an_administrator(self) -> bool {
        matches!(
            self,
            Installer::AsksForAPassword | Installer::EndsTheAppThenAsks
        )
    }
}

/// The updater the schedule drives, and the dialogs it asks through. The
/// schedule decides the order (download, hold, install, relaunch); the
/// source carries each step out. Errors are the updater's words.
#[async_trait]
pub trait UpdateSource: Send + Sync {
    /// Reads the lanes' manifests: the newer version they offer, or `None`
    /// when this build is the newest.
    async fn check(&self) -> Result<Option<String>, String>;
    /// Downloads and verifies `version`. Fails when the last check found
    /// another version, or none.
    async fn download(&self, version: &str) -> Result<Vec<u8>, String>;
    /// Writes `package`, the download of `version`, over the app. Fails when
    /// the last check found another version, or none. On Windows it ends the
    /// process, so it returns only on failure.
    async fn install(&self, version: &str, package: Vec<u8>) -> Result<(), String>;
    /// How [`Self::install`] installs on this build.
    fn installer(&self) -> Installer;
    /// Runs the shutdown and relaunches into the installed version; returns
    /// only in a test.
    async fn relaunch(&self);
    /// Asks the user `question`; true for the answer that installs.
    async fn ask(&self, question: Question<'_>) -> bool;
    /// Tells the user an install failed. One after the install's own
    /// shutdown restarts the app instead; one while the app quits is logged.
    fn tell_install_failed(&self, message: &str);
    /// Tells the user `version` is installed and Steno relaunches once `busy`
    /// has ended.
    fn tell_relaunch_waits(&self, version: &str, busy: Busy);
    /// Offers the found update to the user ([`UpdateSchedule::offer`]).
    fn announce(&self, version: &str);
}

#[derive(Debug)]
struct State {
    last_check_at: Option<DateTime<Utc>>,
    outcome: UpdateOutcome,
    /// While a check runs, Settings' button is off.
    checking: bool,
    /// The newer version the last successful check found.
    found: Option<String>,
    /// The found version still to download, once the app is idle.
    to_download: Option<String>,
    staged: Option<Staged>,
    /// The version last announced or shown, so this run does not ask again.
    announced: Option<String>,
    /// The version an installer that returned installed, while the relaunch
    /// waits.
    installed: Option<String>,
}

struct Staged {
    version: String,
    package: Vec<u8>,
}

impl std::fmt::Debug for Staged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Staged")
            .field("version", &self.version)
            .field("bytes", &self.package.len())
            .finish()
    }
}

/// The host's `Updater`: the daily schedule, the flags and the last check.
pub struct UpdateSchedule {
    me: Weak<UpdateSchedule>,
    source: Arc<dyn UpdateSource>,
    preferences: Arc<dyn Preferences>,
    clock: Arc<dyn Clock>,
    gate: Arc<dyn InstallGate>,
    recorder: Arc<dyn Recorder>,
    last_check_path: PathBuf,
    managed: bool,
    runtime: tokio::runtime::Handle,
    state: Mutex<State>,
    /// One check at a time.
    one_check: tokio::sync::Mutex<()>,
    /// One install at a time, the user's download included.
    installing: AtomicBool,
    on_change: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl std::fmt::Debug for UpdateSchedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateSchedule")
            .field("last_check_path", &self.last_check_path)
            .field("managed", &self.managed)
            .field("state", &*self.state())
            .finish_non_exhaustive()
    }
}

/// What [`UpdateSchedule::new`] is built from.
pub struct ScheduleParts {
    pub source: Arc<dyn UpdateSource>,
    pub preferences: Arc<dyn Preferences>,
    pub clock: Arc<dyn Clock>,
    pub gate: Arc<dyn InstallGate>,
    /// The recorder, whose recording holds back the announcement.
    pub recorder: Arc<dyn Recorder>,
    pub support_directory: PathBuf,
    pub managed: bool,
    pub runtime: tokio::runtime::Handle,
}

impl UpdateSchedule {
    /// Reads the last check time; the schedule waits for [`Self::start`].
    pub fn new(parts: ScheduleParts) -> Arc<Self> {
        let last_check_path = parts.support_directory.join(LAST_CHECK_FILE);
        let last_check_at = read_last_check(&last_check_path);
        Arc::new_cyclic(|me| UpdateSchedule {
            me: me.clone(),
            source: parts.source,
            preferences: parts.preferences,
            clock: parts.clock,
            gate: parts.gate,
            recorder: parts.recorder,
            last_check_path,
            managed: parts.managed,
            runtime: parts.runtime,
            state: Mutex::new(State {
                last_check_at,
                outcome: UpdateOutcome::NotChecked,
                checking: false,
                found: None,
                to_download: None,
                staged: None,
                announced: None,
                installed: None,
            }),
            one_check: tokio::sync::Mutex::new(()),
            installing: AtomicBool::new(false),
            on_change: Mutex::new(None),
        })
    }

    /// Calls `on_change` whenever a check starts or ends.
    pub fn on_change(&self, on_change: Arc<dyn Fn() + Send + Sync>) {
        *lock(&self.on_change) = Some(on_change);
    }

    /// Unless the updates are managed, runs a tick now and every [`TICK`]
    /// on the runtime.
    pub fn start(&self) {
        if self.managed {
            return;
        }
        let Some(schedule) = self.me.upgrade() else {
            return;
        };
        self.runtime.spawn(async move {
            let mut ticks = tokio::time::interval(TICK);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticks.tick().await;
                schedule.tick().await;
            }
        });
    }

    /// One round of the schedule: a due check; with automatic downloads on,
    /// the download and install of a found update once the app is idle; and,
    /// unless a recording is under way, the announcement of one that did not
    /// install.
    pub async fn tick(&self) {
        if self.managed {
            return;
        }
        if self.automatically_checks() {
            self.check_if_due().await;
        }
        if self.automatically_downloads() {
            self.download_when_idle().await;
            if self.install_when_idle().await {
                return;
            }
        }
        self.announce_when_idle();
    }

    /// Whether the schedule would check now: no check yet, the last one
    /// [`CHECK_INTERVAL`] ago or longer, or one in the future.
    pub fn is_due(&self) -> bool {
        let now = self.clock.now();
        match self.state().last_check_at {
            None => true,
            Some(last) => last > now || now - last >= CHECK_INTERVAL,
        }
    }

    /// The user's check: recorded as the schedule's own, and a found update
    /// counts as announced. [`MANAGED_CHECK`] on a packaged install.
    pub async fn check_on_request(&self) -> Result<Option<String>, String> {
        if self.managed {
            return Err(MANAGED_CHECK.to_owned());
        }
        let one = self.one_check.lock().await;
        let result = self.check_holding(one).await;
        if let Ok(Some(version)) = &result {
            self.state().announced = Some(version.clone());
        }
        result
    }

    /// The tick's check, unless the user's check ran meanwhile.
    async fn check_if_due(&self) {
        let one = self.one_check.lock().await;
        if self.is_due() {
            let _ = self.check_holding(one).await;
        }
    }

    /// Downloads the found update when the app is idle and no install runs.
    /// A package whose download ends after automatic downloads were turned
    /// off is not kept.
    async fn download_when_idle(&self) {
        if self.installing.load(Ordering::SeqCst) || !self.gate.is_idle_now() {
            return;
        }
        let Some(version) = self.state().to_download.take() else {
            return;
        };
        match self.source.download(&version).await {
            Ok(package) if self.automatically_downloads() => {
                self.state().staged = Some(Staged { version, package });
            }
            Ok(_) => tracing::info!(%version, "automatic downloads were turned off; not kept"),
            Err(error) => {
                tracing::warn!(%version, "the update could not be downloaded: {error}");
            }
        }
    }

    /// Checks, giving up after [`CHECK_TIMEOUT`]: on success records the time
    /// and what it found, frees a package kept for another version, and
    /// marks a new version to download.
    async fn check_holding(
        &self,
        _one: tokio::sync::MutexGuard<'_, ()>,
    ) -> Result<Option<String>, String> {
        self.change(|state| state.checking = true);
        let result = tokio::time::timeout(CHECK_TIMEOUT, self.source.check())
            .await
            .unwrap_or_else(|_| Err(CHECK_TIMED_OUT.to_owned()));
        let now = self.clock.now();
        self.change(|state| {
            state.checking = false;
            match &result {
                Ok(found) => {
                    state.last_check_at = Some(now);
                    state.outcome = found
                        .clone()
                        .map_or(UpdateOutcome::UpToDate, UpdateOutcome::Available);
                    state
                        .staged
                        .take_if(|staged| Some(&staged.version) != found.as_ref());
                    state.to_download = found.clone().filter(|_| state.staged.is_none());
                    state.found.clone_from(found);
                }
                Err(message) => state.outcome = UpdateOutcome::Failed(message.clone()),
            }
        });
        if result.is_ok() {
            write_last_check(&self.last_check_path, now);
        }
        result
    }

    fn install_failed(&self, message: String) {
        self.source.tell_install_failed(&message);
        self.change(|state| {
            state.staged = None;
            state.outcome = UpdateOutcome::Failed(message);
        });
    }

    /// Asks whether to install `version` now and, on a yes, installs and
    /// relaunches. When the app is busy by the yes it asks whether to install
    /// once that ends ([`Question::AfterItEnds`]); "Not Now" leaves the
    /// version to announce again. Once an update is installed and the
    /// relaunch waits, the user is told so instead.
    pub async fn offer(&self, version: &str) {
        let installed = self.state().installed.clone();
        if let Some(installed) = installed {
            self.tell_relaunch_waits(&installed);
            return;
        }
        if !self.source.ask(Question::Install(version)).await {
            return;
        }
        let busy = self.busy();
        if let Some(busy) = busy
            && !self.source.ask(Question::AfterItEnds(busy)).await
        {
            self.announce_again(version);
            return;
        }
        self.install_on_request(version, busy.is_some()).await;
    }

    /// What keeps an install from running now; `None` while the app is idle.
    fn busy(&self) -> Option<Busy> {
        if self.recording_under_way() {
            Some(Busy::Recording)
        } else if self.gate.is_idle_now() {
            None
        } else {
            Some(Busy::Processing)
        }
    }

    /// The user's install of `version` after the dialog's yes (`put_off` when
    /// given while the app was busy): once idle, from the kept package or a
    /// download, then installed once the gate's hold is taken
    /// ([`Self::hold_for_install`]). A long download counts as put off. A
    /// recording started meanwhile is waited for, never stopped. A newer
    /// version a check found during a wait keeps the yes.
    async fn install_on_request(&self, version: &str, mut put_off: bool) -> Installed {
        let Some(_one) = OneInstall::start(&self.installing) else {
            return Installed::Skipped;
        };
        let mut version = version.to_owned();
        loop {
            put_off |= self.until_idle().await;
            let Some(found) = self.on_offer(&version) else {
                return Installed::Failed;
            };
            version = found;
            let kept = self
                .state()
                .staged
                .take_if(|staged| staged.version == version)
                .map(|staged| staged.package);
            let was_kept = kept.is_some();
            let package = if let Some(package) = kept {
                package
            } else {
                let started = tokio::time::Instant::now();
                let downloaded = self.source.download(&version).await;
                put_off |= started.elapsed() > LONG_DOWNLOAD;
                match downloaded {
                    Ok(package) => package,
                    Err(message) => {
                        self.install_failed(message);
                        return Installed::Failed;
                    }
                }
            };
            let Some(hold) = self.hold_for_install(&version, put_off).await else {
                if was_kept {
                    self.keep(&version, package);
                }
                self.announce_again(&version);
                return Installed::PutOff;
            };
            // A check during the waits may have found a newer version, or none.
            match self.on_offer(&version) {
                Some(found) if found == version => {
                    return self.install_now(&version, package, hold).await;
                }
                Some(_) => put_off = true,
                None => return Installed::Failed,
            }
        }
    }

    /// Keeps `package` again after a "Later", unless a check found another
    /// version or automatic downloads were turned off meanwhile.
    fn keep(&self, version: &str, package: Vec<u8>) {
        let downloads = self.automatically_downloads();
        let mut state = self.state();
        if downloads && state.found.as_deref() == Some(version) {
            state.staged = Some(Staged {
                version: version.to_owned(),
                package,
            });
        }
    }

    /// The version a yes for `version` installs: the last check's update,
    /// then counted as announced. `None` when there is none, and the user is
    /// told.
    fn on_offer(&self, version: &str) -> Option<String> {
        let mut state = self.state();
        let Some(found) = state.found.clone() else {
            drop(state);
            self.install_failed(format!("Steno {version} is no longer the update on offer."));
            return None;
        };
        if found != version {
            tracing::info!(%version, %found, "a yes carried over to the update a later check found");
            state.announced = Some(found.clone());
        }
        Some(found)
    }

    /// Returns once the app is idle, polling every [`IDLE_POLL`]; true when
    /// it had to wait.
    async fn until_idle(&self) -> bool {
        let mut waited = false;
        while !self.gate.is_idle_now() {
            waited = true;
            tokio::time::sleep(IDLE_POLL).await;
        }
        waited
    }

    /// The gate's hold for the user's install. An installer that asks an
    /// administrator runs only right after a yes given while idle: otherwise
    /// it asks again once the app is idle ([`Question::InstallNow`]). `None`
    /// when the answer is no.
    async fn hold_for_install(&self, version: &str, put_off: bool) -> Option<InstallHold> {
        if !self.source.installer().asks_an_administrator() {
            return Some(self.hold_once_idle().await);
        }
        if !put_off && let Some(hold) = self.gate.try_hold() {
            return Some(hold);
        }
        loop {
            self.until_idle().await;
            if !self.source.ask(Question::InstallNow(version)).await {
                return None;
            }
            if let Some(hold) = self.gate.try_hold() {
                return Some(hold);
            }
        }
    }

    /// The gate's hold, once the app is idle: tried at once and then every
    /// [`IDLE_POLL`].
    pub async fn hold_once_idle(&self) -> InstallHold {
        loop {
            if let Some(hold) = self.gate.try_hold() {
                return hold;
            }
            tokio::time::sleep(IDLE_POLL).await;
        }
    }

    /// Installs `package` of `version` and relaunches, with the gate's
    /// `hold`. Where the install ends the app the hold is kept through it.
    /// An installer that returns may wait on a password prompt, so recording
    /// starts go at once and processing jobs after [`JOB_HOLD_LIMIT`]; the
    /// relaunch then takes the hold again ([`Self::hold_to_relaunch`]). A
    /// failed install drops it.
    async fn install_now(
        &self,
        version: &str,
        package: Vec<u8>,
        mut hold: InstallHold,
    ) -> Installed {
        let installed = if self.source.installer().returns() {
            hold.starts = None;
            let mut install = std::pin::pin!(self.source.install(version, package));
            match tokio::time::timeout(JOB_HOLD_LIMIT, &mut install).await {
                Ok(installed) => installed,
                Err(_elapsed) => {
                    hold.jobs = None;
                    install.await
                }
            }
        } else {
            self.source.install(version, package).await
        };
        if let Err(message) = installed {
            drop(hold);
            self.install_failed(message);
            return Installed::Failed;
        }
        let hold = if hold.starts.is_some() {
            hold
        } else {
            // One hold at a time: the jobs' part goes before the new one.
            drop(hold);
            self.state().installed = Some(version.to_owned());
            self.hold_to_relaunch(version).await
        };
        self.source.relaunch().await;
        self.state().installed = None;
        drop(hold);
        Installed::Relaunching
    }

    /// The gate's hold for the relaunch: at once while idle; otherwise the
    /// user is told the relaunch waits.
    async fn hold_to_relaunch(&self, version: &str) -> InstallHold {
        if let Some(hold) = self.gate.try_hold() {
            return hold;
        }
        self.tell_relaunch_waits(version);
        self.hold_once_idle().await
    }

    /// Tells the user the relaunch waits, and for what; nothing while idle or
    /// shutting down.
    fn tell_relaunch_waits(&self, version: &str) {
        if self.gate.is_shutting_down() {
            return;
        }
        if let Some(busy) = self.busy() {
            self.source.tell_relaunch_waits(version, busy);
        }
    }

    fn recording_under_way(&self) -> bool {
        under_way(self.recorder.status().state)
    }

    /// The user put the install of `version` off: it is announced again at
    /// the first tick with no recording under way.
    pub fn announce_again(&self, version: &str) {
        self.state()
            .announced
            .take_if(|announced| *announced == version);
    }

    /// Installs the kept package while holding the gate; true when the
    /// relaunch is under way. Not for an installer that asks an
    /// administrator, whose kept package is announced instead.
    async fn install_when_idle(&self) -> bool {
        if self.state().staged.is_none() || self.source.installer().asks_an_administrator() {
            return false;
        }
        let Some(_one) = OneInstall::start(&self.installing) else {
            return false;
        };
        let Some(hold) = self.gate.try_hold() else {
            return false;
        };
        let Some(staged) = self.state().staged.take() else {
            return false;
        };
        self.install_now(&staged.version, staged.package, hold)
            .await
            == Installed::Relaunching
    }

    /// Announces the found update unless already shown in this run or a
    /// recording is under way.
    fn announce_when_idle(&self) {
        if self.recording_under_way() {
            return;
        }
        let mut state = self.state();
        let Some(version) = state
            .found
            .clone()
            .filter(|found| state.announced.as_ref() != Some(found))
        else {
            return;
        };
        state.announced = Some(version.clone());
        drop(state);
        self.source.announce(&version);
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Applies `body` to the state, then tells the host, outside the lock.
    fn change(&self, body: impl FnOnce(&mut State)) {
        body(&mut self.state());
        let on_change = lock(&self.on_change).clone();
        if let Some(on_change) = on_change {
            on_change();
        }
    }
}

impl Updater for UpdateSchedule {
    fn can_check_for_updates(&self) -> bool {
        !self.managed && !self.state().checking
    }

    fn automatically_checks(&self) -> bool {
        self.preferences
            .stored_flag(AUTOMATIC_CHECKS_KEY)
            .unwrap_or(AUTOMATIC_CHECKS_DEFAULT)
    }

    fn set_automatically_checks(&self, enabled: bool) {
        self.preferences.set_flag(AUTOMATIC_CHECKS_KEY, enabled);
    }

    fn automatically_downloads(&self) -> bool {
        self.preferences
            .stored_flag(AUTOMATIC_DOWNLOAD_KEY)
            .unwrap_or(AUTOMATIC_DOWNLOAD_DEFAULT)
    }

    /// Turning automatic downloads off frees any kept package.
    fn set_automatically_downloads(&self, enabled: bool) {
        self.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, enabled);
        if !enabled {
            self.state().staged = None;
        }
    }

    fn last_check_at(&self) -> Option<DateTime<Utc>> {
        self.state().last_check_at
    }

    fn last_outcome(&self) -> UpdateOutcome {
        self.state().outcome.clone()
    }

    /// The host's answer to `updates.check`, for a host without a shell; the
    /// desktop shell answers it itself with its dialogs.
    fn check_for_updates(&self) {
        if self.managed {
            return;
        }
        let Some(schedule) = self.me.upgrade() else {
            return;
        };
        self.runtime.spawn(async move {
            if let Ok(Some(version)) = schedule.check_on_request().await {
                schedule.source.announce(&version);
            }
        });
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct OneInstall<'a>(&'a AtomicBool);

impl<'a> OneInstall<'a> {
    fn start(installing: &'a AtomicBool) -> Option<Self> {
        (!installing.swap(true, Ordering::SeqCst)).then_some(OneInstall(installing))
    }
}

impl Drop for OneInstall<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// [`LAST_CHECK_FILE`]'s one key.
#[derive(Serialize, Deserialize)]
struct LastCheck {
    #[serde(rename = "lastCheckAt")]
    last_check_at: String,
}

/// The time in `path`; `None` when there is no file or it holds none.
fn read_last_check(path: &Path) -> Option<DateTime<Utc>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!("{} could not be read: {error}", path.display());
            return None;
        }
    };
    let parsed = serde_json::from_slice::<LastCheck>(&bytes)
        .map_err(|error| error.to_string())
        .and_then(|file| {
            DateTime::parse_from_rfc3339(&file.last_check_at).map_err(|error| error.to_string())
        });
    match parsed {
        Ok(time) => Some(time.with_timezone(&Utc)),
        Err(error) => {
            tracing::warn!("{} holds no check time: {error}", path.display());
            None
        }
    }
}

fn write_last_check(path: &Path, at: DateTime<Utc>) {
    let file = LastCheck {
        last_check_at: at.to_rfc3339_opts(SecondsFormat::Secs, true),
    };
    let written = serde_json::to_vec(&file)
        .map_err(std::io::Error::other)
        .and_then(|bytes| replace_file(path, &bytes, Access::Default));
    if let Err(error) = written {
        tracing::warn!("{} could not be written: {error}", path.display());
    }
}

#[cfg(test)]
mod tests;
