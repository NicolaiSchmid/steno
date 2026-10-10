//! The update schedule: the host's `Updater` over the shell's update
//! source (the Tauri updater, `apps/desktop/src-tauri/src/updater.rs`).
//! Swift: `apps/macos/Steno/Services/UpdaterController.swift`, where
//! Sparkle checked daily (`SUScheduledCheckInterval` 86400 in
//! `apps/macos/project.yml`).
//!
//! - **The schedule.** [`UpdateSchedule::start`] runs a [`UpdateSchedule::tick`]
//!   at launch and every hour ([`TICK`]). A tick checks when automatic
//!   checks are on and the last check time is missing, more than a day old
//!   ([`CHECK_INTERVAL`]) or in the future (the clock was set back). A check
//!   that has not answered after [`CHECK_TIMEOUT`] fails, so a stalled
//!   request cannot hold the next check or Settings' button.
//! - **The flags** are the booleans [`AUTOMATIC_CHECKS_KEY`] and
//!   [`AUTOMATIC_DOWNLOAD_KEY`] in `preferences.json`, where the Mac's
//!   first launch copies Sparkle's `SUEnableAutomaticChecks` and
//!   `SUAutomaticallyUpdate`. A missing key reads as Sparkle's value for a
//!   fresh install: `SUEnableAutomaticChecks` is true in the Swift app's
//!   Info.plist, and Sparkle does not download by itself unless asked
//!   ([`AUTOMATIC_CHECKS_DEFAULT`], [`AUTOMATIC_DOWNLOAD_DEFAULT`]).
//! - **The last check time** is the RFC 3339 UTC string under
//!   [`LAST_CHECK_KEY`] in [`LAST_CHECK_FILE`] beside `preferences.json`,
//!   written whole with [`replace_file`] after a check that succeeded,
//!   whether or not it found an update; a failed check leaves it as it was,
//!   so the next hourly tick tries again. A file that cannot be read or
//!   parsed reads as no check yet. Swift: Sparkle's `SULastCheckTime`.
//! - **A found update** is announced once per version in a run
//!   ([`UpdateSource::announce`]), as Sparkle's update alert, but not while
//!   a recording starts, runs or stops; the first tick after it ends
//!   announces it. The user installs it from there
//!   ([`UpdateSchedule::offer`]): a yes given while a recording or a
//!   processing job runs asks once more, whether to install once it ends
//!   ([`Question::AfterItEnds`]), and "Not Now" there leaves the version to
//!   announce again ([`UpdateSchedule::announce_again`]).
//! - **An install never stops a recording or a processing job** (stable
//!   plan P25). It installs only while it holds the [`InstallGate`]'s
//!   [`InstallHold`], which the app's gate ([`IdleGate`]) gives only while
//!   no recording starts, runs or stops (its save runs while it stops),
//!   no processing job runs or waits to, and the app is not shutting down.
//!   The hold is kept through the install, the shutdown and the relaunch:
//!   meanwhile a Record is refused and says
//!   [`INSTALLING_UPDATE`](steno_host::services::INSTALLING_UPDATE), and a
//!   processing job, such as one for a phone recording that arrives, waits
//!   `queued`. The download comes first, unless the package is kept, and
//!   only while the app is idle ([`InstallGate::is_idle_now`]), since a
//!   recording or a processing job has the disk and the network to itself.
//!   The user's install waits for both, looking again every [`IDLE_POLL`]
//!   ([`UpdateSchedule::wait_until_idle`]). One install runs at a time,
//!   and only of the version the last check found.
//! - **Automatic downloads**: a found update is downloaded at the first
//!   tick that finds the app idle, and installed at the first tick that
//!   gets the gate's hold; while the app is busy each waits for a later
//!   tick. The flag is read again when the download ends and once the
//!   install holds the gate, so a switch turned off during the transfer
//!   keeps and installs nothing. The schedule keeps the downloaded package
//!   until an install takes it, and frees it when a check finds another
//!   version, or none, and when automatic downloads are turned off.
//! - **Packaged installs** ([`updates_are_managed`], stable plan X5): no
//!   schedule, and no check, the user's included.
//!
//! The network is the updater's: the schedule reads the same lane manifests
//! as the tray's Check for Updates, and sends nothing else.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use steno_bridge::RecordingState;
use steno_host::services::{Clock, Preferences, Recorder, UpdateOutcome, Updater};

use crate::files::{Access, replace_file};
use crate::pipeline::CurrentPipeline;

/// Whether the schedule checks by itself. Swift: Sparkle's
/// `SUEnableAutomaticChecks`.
pub const AUTOMATIC_CHECKS_KEY: &str = "steno.updates.automaticChecks";
/// Whether a found update is downloaded and installed by itself once the
/// install gate allows it (P25). Swift: Sparkle's `SUAutomaticallyUpdate`.
pub const AUTOMATIC_DOWNLOAD_KEY: &str = "steno.updates.automaticDownload";
/// [`AUTOMATIC_CHECKS_KEY`] when `preferences.json` lacks it: the Swift
/// app's Info.plist set `SUEnableAutomaticChecks`.
pub const AUTOMATIC_CHECKS_DEFAULT: bool = true;
/// [`AUTOMATIC_DOWNLOAD_KEY`] when `preferences.json` lacks it: Sparkle's
/// own default, which the Swift app kept.
pub const AUTOMATIC_DOWNLOAD_DEFAULT: bool = false;
/// The file under the support directory that holds the last check time.
pub const LAST_CHECK_FILE: &str = "update-check.json";
/// The key in [`LAST_CHECK_FILE`]: an RFC 3339 time in UTC
/// (`2026-10-09T08:00:00Z`).
pub const LAST_CHECK_KEY: &str = "lastCheckAt";
/// How old the last check may grow before the schedule checks again.
/// Swift: `SUScheduledCheckInterval` 86400.
pub const CHECK_INTERVAL: TimeDelta = TimeDelta::days(1);
/// How often the schedule looks at the last check time.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(60 * 60);
/// How long a check may take before it fails. Without it a stalled request
/// (a captive portal, a connection left dead by sleep) would hold the one
/// check at a time, every later tick and Settings' button until quit. The
/// updater's own timeout would cap the download as well, so the limit
/// wraps the check alone.
pub const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// How often the user's install, waiting for a recording or a processing
/// job to end, looks again ([`UpdateSchedule::wait_until_idle`]).
pub const IDLE_POLL: std::time::Duration = std::time::Duration::from_secs(2);
/// The outcome of a check that ran into [`CHECK_TIMEOUT`].
pub const CHECK_TIMED_OUT: &str = "The update check timed out.";
/// The outcome of a check asked for on a packaged install: the line the
/// General section shows there too
/// (`steno_host::settings::snapshots::MANAGED_UPDATES`).
pub const MANAGED_CHECK: &str = steno_host::settings::snapshots::MANAGED_UPDATES;

/// The variable a package sets to say it delivers the updates
/// ([`updates_are_managed`]).
pub const DISTRIBUTION_VARIABLE: &str = "STENO_DISTRIBUTION";

/// Whether a package manager delivers this app's updates (stable plan X5):
/// [`DISTRIBUTION_VARIABLE`] is `aur` or `nix`. The environment (the AUR
/// wrapper) wins over the value the build was given (the Nix package), so a
/// Nix build can be told otherwise at run time.
pub fn updates_are_managed() -> bool {
    managed_by(
        std::env::var(DISTRIBUTION_VARIABLE).ok().as_deref(),
        option_env!("STENO_DISTRIBUTION"),
    )
}

fn managed_by(environment: Option<&str>, build: Option<&str>) -> bool {
    matches!(environment.or(build), Some("aur" | "nix"))
}

/// What an install holds while it installs and relaunches: until it is
/// dropped no recording starts and no processing job begins. [`IdleGate`]'s
/// is the recorder's [`Recorder::hold_starts`] and the pipelines'
/// [`JobHold`](steno_pipeline::JobHold).
pub type InstallHold = Box<dyn Send>;

/// Whether an update may install and relaunch the app now (stable plan
/// P25); the app's is [`IdleGate`]. Rust only: Sparkle installed at quit.
pub trait InstallGate: Send + Sync {
    /// A hold when the app is idle (no recording starting, running or
    /// stopping, no save in progress, no processing job, no shutdown),
    /// `None` while it is busy. One hold lives at a time.
    fn try_hold(&self) -> Option<InstallHold>;
    /// Whether the app is idle now, without keeping a hold: a download
    /// waits for it, since a recording or a processing job has the disk
    /// and the network to itself. A gate may answer it more cheaply.
    fn is_idle_now(&self) -> bool {
        self.try_hold().is_some()
    }
}

/// The app's [`InstallGate`] (stable plan P25), over the recorder and the
/// pipelines' in-flight set ([`InFlight`](steno_pipeline::InFlight)), which
/// every reload shares. The app is idle when no recording starts, runs or
/// stops (a stop saves the recording before the recorder is idle, and by
/// then its processing job is claimed), the app is not shutting down
/// ([`CurrentPipeline::quit`]), and no processing job runs or waits to (a
/// meeting waiting for models waits for the user, so it does not count).
/// Rust only.
pub struct IdleGate {
    recorder: Arc<dyn Recorder>,
    pipeline: Arc<CurrentPipeline>,
}

impl IdleGate {
    /// The gate over the app's recorder and pipeline.
    #[must_use]
    pub fn new(recorder: Arc<dyn Recorder>, pipeline: Arc<CurrentPipeline>) -> Self {
        IdleGate { recorder, pipeline }
    }
}

impl InstallGate for IdleGate {
    /// Takes the recorder's start hold, then reads its status, so no
    /// recording starts after the read; then the pipelines' job hold,
    /// whose check that nothing is in flight is one step with the hold. A
    /// busy app is told apart first without a start hold, so a Record
    /// clicked while a job runs is never refused for an install that is
    /// not coming.
    fn try_hold(&self) -> Option<InstallHold> {
        if !self.is_idle_now() {
            return None;
        }
        let starts = self.recorder.hold_starts();
        if stops_a_recording(self.recorder.status().state) || self.pipeline.quitting() {
            return None;
        }
        let jobs = self.pipeline.in_flight().try_hold()?;
        Some(Box::new((starts, jobs)))
    }

    fn is_idle_now(&self) -> bool {
        !stops_a_recording(self.recorder.status().state)
            && !self.pipeline.quitting()
            && self.pipeline.in_flight().is_idle()
    }
}

/// Whether a recording is under way: one is starting, running or
/// stopping. The announcement waits while it does, and no install runs.
fn stops_a_recording(state: RecordingState) -> bool {
    state != RecordingState::Idle
}

/// What the user's install asks ([`UpdateSource::ask`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Question<'a> {
    /// The update alert for this version: install it now and relaunch?
    Install(&'a str),
    /// A yes given while the app is busy: install once that ends? The
    /// answer that installs then is the default; neither answer stops the
    /// recording or the processing.
    AfterItEnds(Busy),
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
    /// The install, or its download, failed; the user is told.
    Failed,
    /// Another install runs, or the last check found another version,
    /// whose own dialog asks, so nothing was installed.
    Skipped,
}

/// The updater the schedule drives, and the dialogs it asks through; the
/// shell's runs the Tauri updater over the release lanes. The schedule
/// decides the order (download, start hold, install, relaunch); the source
/// only carries each step out. Errors are the updater's words, which the
/// General section shows.
#[async_trait]
pub trait UpdateSource: Send + Sync {
    /// Reads the lanes' manifests: the newer version they offer, or `None`
    /// when this build is the newest.
    async fn check(&self) -> Result<Option<String>, String>;
    /// Downloads and verifies `version`, the update the last check found:
    /// the package an install writes. Fails when the last check found
    /// another version, or none.
    async fn download(&self, version: &str) -> Result<Vec<u8>, String>;
    /// Writes `package`, the download of `version`, over the app. Fails
    /// when the last check found another version, or none. On Windows the
    /// installer ends the process once it runs (its exit runs the
    /// shutdown), so it returns there only when it failed.
    async fn install(&self, version: &str, package: Vec<u8>) -> Result<(), String>;
    /// Runs the shutdown and relaunches into the installed version; returns
    /// only in a test.
    async fn relaunch(&self);
    /// Asks the user `question`; true for the answer that installs.
    async fn ask(&self, question: Question<'_>) -> bool;
    /// Tells the user an install failed, with the updater's `message`.
    fn tell_install_failed(&self, message: &str);
    /// Offers the found update to the user ([`UpdateSchedule::offer`]).
    /// Swift: Sparkle's update alert.
    fn announce(&self, version: &str);
}

#[derive(Debug)]
struct State {
    last_check_at: Option<DateTime<Utc>>,
    outcome: UpdateOutcome,
    /// While a check runs, Settings' button is off (Sparkle's
    /// `canCheckForUpdates`); `one_check` makes a second check wait.
    checking: bool,
    /// The newer version the last successful check found.
    found: Option<String>,
    /// The found version still to download, once the app is idle; taken
    /// by the attempt, so a failed download or install waits for the next
    /// check.
    to_download: Option<String>,
    /// The package [`UpdateSource::download`] fetched, or that a put-off
    /// install handed back, until an install takes it.
    staged: Option<Staged>,
    /// The version last announced or shown to the user, so a later tick in
    /// this run does not ask again; a relaunch asks again, as Sparkle
    /// re-alerted at each scheduled check.
    announced: Option<String>,
}

/// A downloaded and verified package and its version.
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
/// See the module doc.
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
    /// One check at a time: a tick that waited behind the user's check
    /// finds it no longer due.
    one_check: tokio::sync::Mutex<()>,
    /// One install at a time ([`OneInstall`]), the download of the user's
    /// included, so two dialogs answered yes do not write two packages.
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
    /// The graph's `preferences.json`, shared with the host.
    pub preferences: Arc<dyn Preferences>,
    pub clock: Arc<dyn Clock>,
    pub gate: Arc<dyn InstallGate>,
    /// The recorder, whose recording holds back the announcement and is
    /// what the user's install says it waits for.
    pub recorder: Arc<dyn Recorder>,
    /// Where [`LAST_CHECK_FILE`] lives.
    pub support_directory: PathBuf,
    /// [`updates_are_managed`] in the product.
    pub managed: bool,
    /// Where the schedule and the host's checks run.
    pub runtime: tokio::runtime::Handle,
}

impl UpdateSchedule {
    /// Reads the last check time from [`LAST_CHECK_FILE`]; the schedule
    /// waits for [`Self::start`].
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
            }),
            one_check: tokio::sync::Mutex::new(()),
            installing: AtomicBool::new(false),
            on_change: Mutex::new(None),
        })
    }

    /// Calls `on_change` from now on whenever a check starts or ends (the
    /// host's `updates_changed`).
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

    /// One round of the schedule: when a check is due, the check; with
    /// automatic downloads on, the download of a found update and its
    /// install, each once the gate says the app is idle; last, unless a
    /// recording is under way, the announcement of a found update that
    /// did not install.
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

    /// The user's check, from the tray or from Settings: records it as the
    /// schedule's own, and counts a found update as announced, since the
    /// caller shows it. [`MANAGED_CHECK`] on a packaged install, without a
    /// request.
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

    /// The tick's check, once it holds `one_check`: none when it is no
    /// longer due because the user's check ran meanwhile.
    async fn check_if_due(&self) {
        let one = self.one_check.lock().await;
        if self.is_due() {
            let _ = self.check_holding(one).await;
        }
    }

    /// Downloads the found update when the app is idle; while it is busy
    /// the download waits for a later tick. A package whose download ends
    /// after automatic downloads were turned off is not kept.
    async fn download_when_idle(&self) {
        if !self.gate.is_idle_now() {
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

    /// Checks, giving up after [`CHECK_TIMEOUT`]: records the outcome and,
    /// when the check succeeded, the time and what it found; a package kept
    /// for another version is freed, and a found version not kept is to
    /// download.
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

    /// An install failed, the user's or the schedule's: the General section
    /// says so, and the user is told.
    fn install_failed(&self, message: String) {
        self.source.tell_install_failed(&message);
        self.change(|state| {
            state.staged = None;
            state.outcome = UpdateOutcome::Failed(message);
        });
    }

    /// Asks whether to install `version` now and, when the user agrees,
    /// installs and relaunches as the module doc's "An install" says:
    /// after the tray's check, and when the schedule announced an update it
    /// does not install by itself. When the app is busy by the yes (the
    /// dialog may have been open since before a recording started) it asks
    /// whether to install once that ends ([`Question::AfterItEnds`]), and
    /// a yes there waits for it ([`Self::wait_until_idle`]); "Not Now", or
    /// closing that dialog, leaves the version to announce again once the
    /// recording has ended. Swift: Sparkle's update alert.
    pub async fn offer(&self, version: &str) {
        if !self.source.ask(Question::Install(version)).await {
            return;
        }
        if let Some(busy) = self.busy()
            && !self.source.ask(Question::AfterItEnds(busy)).await
        {
            self.announce_again(version);
            return;
        }
        self.install_on_request(version).await;
    }

    /// What keeps an install from running now; `None` while the app is
    /// idle.
    fn busy(&self) -> Option<Busy> {
        if self.recording_under_way() {
            Some(Busy::Recording)
        } else if self.gate.is_idle_now() {
            None
        } else {
            Some(Busy::Processing)
        }
    }

    /// The user's install of `version`, after the dialog's yes: once the
    /// app is idle, from the kept package when it is that version, else
    /// downloaded then; then, once the app is idle again and the gate's
    /// hold is taken ([`Self::wait_until_idle`]), installed as
    /// [`Self::install_now`] does. A recording started during the download
    /// is waited for, never stopped. Nothing installs while another
    /// install runs (a second yes meanwhile does nothing), or when the
    /// last check found another version, whose own dialog asks; a version
    /// no longer found fails with a message.
    async fn install_on_request(&self, version: &str) -> Installed {
        let Some(_one) = OneInstall::start(&self.installing) else {
            return Installed::Skipped;
        };
        if let Some(refused) = self.not_on_offer(version) {
            return refused;
        }
        self.until_idle().await;
        let kept = self
            .state()
            .staged
            .take_if(|staged| staged.version == version)
            .map(|staged| staged.package);
        let package = match kept {
            Some(package) => package,
            None => match self.source.download(version).await {
                Ok(package) => package,
                Err(message) => {
                    self.install_failed(message);
                    return Installed::Failed;
                }
            },
        };
        let hold = self.wait_until_idle().await;
        // A check during the waits may have found another version, or none.
        if let Some(refused) = self.not_on_offer(version) {
            return refused;
        }
        self.install_now(version, package, hold).await
    }

    /// Why the user's install of `version` does not go ahead, if it does
    /// not: the last check found another version (skipped, since that one
    /// has its own dialog) or none (failed, with a message).
    fn not_on_offer(&self, version: &str) -> Option<Installed> {
        let found = self.state().found.clone();
        match found {
            Some(found) if found == version => None,
            Some(found) => {
                tracing::info!(%version, %found, "a yes for an update the last check replaced");
                Some(Installed::Skipped)
            }
            None => {
                self.install_failed(format!("Steno {version} is no longer the update on offer."));
                Some(Installed::Failed)
            }
        }
    }

    /// Returns once the gate says the app is idle, looking again every
    /// [`IDLE_POLL`].
    async fn until_idle(&self) {
        while !self.gate.is_idle_now() {
            tokio::time::sleep(IDLE_POLL).await;
        }
    }

    /// The gate's hold, once the app is idle: tried at once and then every
    /// [`IDLE_POLL`], so it waits for a recording to end and be saved and
    /// for every processing job, and for an install that holds the gate
    /// already (the schedule's, which then relaunches or, when it fails,
    /// drops it). What the user's install waits for after its download.
    pub async fn wait_until_idle(&self) -> InstallHold {
        loop {
            if let Some(hold) = self.gate.try_hold() {
                return hold;
            }
            tokio::time::sleep(IDLE_POLL).await;
        }
    }

    /// Installs the downloaded `package` of `version` and relaunches, while
    /// `hold`, the gate's, keeps recording starts and processing jobs off
    /// through the install, the shutdown and the relaunch. The hold is
    /// taken before the install, not only before the relaunch, since on
    /// Windows the installer ends the process itself. It spans the
    /// updater's password prompt where it asks one (a `.deb` install
    /// always does), so recording stays refused until the user answers or
    /// cancels it; a cancel fails the install, which drops the hold.
    async fn install_now(&self, version: &str, package: Vec<u8>, hold: InstallHold) -> Installed {
        match self.source.install(version, package).await {
            Ok(()) => {
                self.source.relaunch().await;
                drop(hold);
                Installed::Relaunching
            }
            Err(message) => {
                drop(hold);
                self.install_failed(message);
                Installed::Failed
            }
        }
    }

    /// Whether a recording is starting, running or stopping now
    /// ([`stops_a_recording`]).
    fn recording_under_way(&self) -> bool {
        stops_a_recording(self.recorder.status().state)
    }

    /// The user put the install of `version` off ("Not Now" while the app
    /// was busy): the first idle tick after the recording ends announces
    /// it again.
    pub fn announce_again(&self, version: &str) {
        self.state()
            .announced
            .take_if(|announced| *announced == version);
    }

    /// Installs the kept package while holding the gate, unless automatic
    /// downloads were turned off meanwhile; true when the relaunch is
    /// under way. With nothing kept it takes no hold.
    async fn install_when_idle(&self) -> bool {
        if self.state().staged.is_none() {
            return false;
        }
        let Some(hold) = self.gate.try_hold() else {
            return false;
        };
        let Some(_one) = OneInstall::start(&self.installing) else {
            return false;
        };
        let Some(staged) = self.state().staged.take() else {
            return false;
        };
        if !self.automatically_downloads() {
            return false;
        }
        self.install_now(&staged.version, staged.package, hold)
            .await
            == Installed::Relaunching
    }

    /// Announces the found update unless it was announced or shown in this
    /// run, or a recording is under way: the dialog would offer to end the
    /// recording, so a later tick announces it instead.
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

    /// Turning automatic downloads off frees any kept package, a put-off
    /// install's included, which the next yes downloads again.
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

    /// The host's answer to `updates.check`, for a host without a shell:
    /// checks on the runtime ([`UpdateSchedule::check_on_request`]) and
    /// announces a found update. The desktop shell answers `updates.check`
    /// itself with its dialogs (`bridge.rs`, `updater::check_and_offer`).
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

/// An install under way: while it lives a second one returns at once.
struct OneInstall<'a>(&'a AtomicBool);

impl<'a> OneInstall<'a> {
    /// `None` while another install runs.
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

/// The time in `path`; `None` when there is no file or it does not hold
/// one, which a later check overwrites.
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
