//! The update schedule: the host's `Updater` over the shell's update
//! source (the Tauri updater, `apps/desktop/src-tauri/src/updater.rs`).
//! Swift: `UpdaterController.swift`, where Sparkle checked daily
//! (`SUScheduledCheckInterval` 86400 in `apps/macos/project.yml`).
//!
//! - **The schedule.** [`UpdateSchedule::start`] runs a [`UpdateSchedule::tick`]
//!   at launch and every hour ([`TICK`]). A tick checks when automatic
//!   checks are on and the last check time is missing, more than a day old
//!   ([`CHECK_INTERVAL`]) or in the future (the clock was set back).
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
//! - **A found update** is downloaded when automatic downloads are on, and
//!   installed only through the [`InstallGate`] (stable plan P25): the
//!   install holds the gate's [`InstallHold`] through the install, the
//!   shutdown and the relaunch, and without a hold the download waits for a
//!   later tick. An update that does not install is announced once per
//!   version ([`UpdateSource::announce`]), as Sparkle's update alert, and the
//!   user installs it from there.
//! - **Packaged installs** ([`updates_are_managed`], stable plan X5): no
//!   schedule and no check.
//!
//! The network is the updater's: the schedule asks the same lane manifests
//! the tray's Check for Updates asks, and sends nothing else.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use steno_host::services::{Clock, Preferences, UpdateOutcome, Updater};

use crate::files::{Access, replace_file};

/// Whether the schedule checks by itself. Swift: Sparkle's
/// `SUEnableAutomaticChecks`.
pub const AUTOMATIC_CHECKS_KEY: &str = "steno.updates.automaticChecks";
/// Whether a found update is downloaded and installed without asking.
/// Swift: Sparkle's `SUAutomaticallyUpdate`.
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
/// dropped no recording starts, no save runs and no processing job begins.
pub type InstallHold = Box<dyn Send>;

/// Whether an update may install and relaunch the app now (stable plan
/// P25). Rust only: Sparkle installed at quit.
pub trait InstallGate: Send + Sync {
    /// A hold when the app is idle (no recording starting, running or
    /// stopping, no save in progress, no processing job), `None` while it
    /// is busy.
    fn try_hold(&self) -> Option<InstallHold>;
}

/// The gate until P25 builds the real one: never idle, so the schedule
/// never installs or relaunches by itself, and a found update waits for
/// the user to install it.
#[derive(Debug, Default, Clone, Copy)]
pub struct NeverIdle;

impl InstallGate for NeverIdle {
    fn try_hold(&self) -> Option<InstallHold> {
        None
    }
}

/// The updater the schedule drives; the shell's runs the Tauri updater
/// over the release lanes. Errors are the updater's words, which the
/// General section shows.
#[async_trait]
pub trait UpdateSource: Send + Sync {
    /// Reads the lanes' manifests: the newer version they offer, or `None`
    /// when this build is the newest.
    async fn check(&self) -> Result<Option<String>, String>;
    /// Downloads and verifies the update the last check found and keeps it
    /// for the install.
    async fn download(&self) -> Result<(), String>;
    /// Installs the update [`Self::download`] kept, runs the shutdown and
    /// relaunches; returns only when the install failed.
    async fn install_and_relaunch(&self) -> Result<(), String>;
    /// Offers the found update to the user, who may install it then.
    /// Swift: Sparkle's update alert.
    fn announce(&self, version: &str);
}

#[derive(Debug)]
struct State {
    last_check_at: Option<DateTime<Utc>>,
    outcome: UpdateOutcome,
    /// While a check runs, no other starts (Sparkle's
    /// `canCheckForUpdates`).
    checking: bool,
    /// The version [`UpdateSource::download`] kept.
    staged: Option<String>,
    /// The version last announced, so an hourly tick does not ask again.
    announced: Option<String>,
}

/// The host's `Updater`: the daily schedule, the flags and the last check.
/// See the module doc.
pub struct UpdateSchedule {
    me: Weak<UpdateSchedule>,
    source: Arc<dyn UpdateSource>,
    preferences: Arc<dyn Preferences>,
    clock: Arc<dyn Clock>,
    gate: Arc<dyn InstallGate>,
    last_check_path: PathBuf,
    managed: bool,
    runtime: tokio::runtime::Handle,
    state: Mutex<State>,
    one_check: tokio::sync::Mutex<()>,
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
            last_check_path,
            managed: parts.managed,
            runtime: parts.runtime,
            state: Mutex::new(State {
                last_check_at,
                outcome: UpdateOutcome::NotChecked,
                checking: false,
                staged: None,
                announced: None,
            }),
            one_check: tokio::sync::Mutex::new(()),
            on_change: Mutex::new(None),
        })
    }

    /// Calls `on_change` whenever a check starts or ends (the host's
    /// `updates_changed`), and, unless the updates are managed, runs a tick
    /// now and every [`TICK`] on the runtime.
    pub fn start(&self, on_change: Arc<dyn Fn() + Send + Sync>) {
        *lock(&self.on_change) = Some(on_change);
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

    /// One round of the schedule: a download a busy app held back installs
    /// once the gate gives a hold; then, when a check is due, the check,
    /// and for a found update the download and the install (automatic
    /// downloads on) or the announcement.
    pub async fn tick(&self) {
        if self.managed {
            return;
        }
        let downloads = self.automatically_downloads();
        if downloads && self.state().staged.is_some() && self.install_when_idle().await {
            return;
        }
        if !self.automatically_checks() || !self.is_due() {
            return;
        }
        let Ok(Some(version)) = self.check_now().await else {
            return;
        };
        if downloads {
            match self.source.download().await {
                Ok(()) => {
                    self.state().staged = Some(version.clone());
                    if self.install_when_idle().await {
                        return;
                    }
                }
                Err(error) => {
                    tracing::warn!(%version, "the update could not be downloaded: {error}");
                }
            }
        }
        let first = {
            let mut state = self.state();
            let first = state.announced.as_deref() != Some(version.as_str());
            state.announced = Some(version.clone());
            first
        };
        if first {
            self.source.announce(&version);
        }
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

    /// Checks now, the schedule's check and the user's alike: records the
    /// outcome and, when the check succeeded, the time. One check at a
    /// time; a second waits for the first.
    pub async fn check_now(&self) -> Result<Option<String>, String> {
        let _one = self.one_check.lock().await;
        self.change(|state| state.checking = true);
        let result = self.source.check().await;
        let now = self.clock.now();
        self.change(|state| {
            state.checking = false;
            match &result {
                Ok(found) => {
                    state.last_check_at = Some(now);
                    state.outcome = found
                        .clone()
                        .map_or(UpdateOutcome::UpToDate, UpdateOutcome::Available);
                }
                Err(message) => state.outcome = UpdateOutcome::Failed(message.clone()),
            }
        });
        if result.is_ok() {
            write_last_check(&self.last_check_path, now);
        }
        result
    }

    /// An install the user asked for failed: the General section says so.
    pub fn install_failed(&self, message: String) {
        self.change(|state| {
            state.staged = None;
            state.outcome = UpdateOutcome::Failed(message);
        });
    }

    /// Installs the kept download while holding the gate; true when the
    /// relaunch is under way.
    async fn install_when_idle(&self) -> bool {
        let Some(hold) = self.gate.try_hold() else {
            return false;
        };
        let installed = self.source.install_and_relaunch().await;
        drop(hold);
        match installed {
            Ok(()) => true,
            Err(message) => {
                tracing::warn!("the downloaded update could not be installed: {message}");
                self.install_failed(message);
                false
            }
        }
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

    fn set_automatically_downloads(&self, enabled: bool) {
        self.preferences.set_flag(AUTOMATIC_DOWNLOAD_KEY, enabled);
    }

    fn last_check_at(&self) -> Option<DateTime<Utc>> {
        self.state().last_check_at
    }

    fn last_outcome(&self) -> UpdateOutcome {
        self.state().outcome.clone()
    }

    /// Checks on the runtime and announces a found update; the host's
    /// `updates.check` (the shell answers that one itself, with its
    /// dialogs).
    fn check_for_updates(&self) {
        if self.managed {
            return;
        }
        let Some(schedule) = self.me.upgrade() else {
            return;
        };
        self.runtime.spawn(async move {
            if let Ok(Some(version)) = schedule.check_now().await {
                schedule.source.announce(&version);
            }
        });
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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
