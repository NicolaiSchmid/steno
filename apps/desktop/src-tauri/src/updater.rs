//! Updates over `tauri-plugin-updater`: a signed manifest per lane on the
//! GitHub release, checked on request from the tray or from Settings
//! (`updates.check`), and daily by the update schedule
//! (`steno_services::updates`), which drives [`ShellUpdates`], records each
//! check's outcome and time for the General section, and downloads and
//! installs by itself only through its install gate (stable plan P25). The
//! lane follows the installed version, as Sparkle's channel does in the
//! Swift app: a pre-release build ("0.11.0-rc.1") reads the beta lane's
//! manifest first and falls back to the stable one; a stable build reads
//! the stable manifest only. No setting.
//!
//! Each lane is a rolling GitHub release that holds only its `latest.json`,
//! which `.github/workflows/desktop-release.yml` replaces when it publishes
//! a `desktop-v*` tag. The stable lane takes releases, the beta lane every
//! version, and neither moves backwards (`apps/desktop/scripts/updater-lanes.sh`),
//! so a beta build is offered the stable release that follows it. The
//! manifest points at the installers on the versioned release. Neither
//! lane is GitHub's "latest" release, which stays the Swift app's until the
//! Mac cutover (`.plans/2026-10-04-mac-cutover.md`). `tauri.conf.json`
//! carries the public key (`plugins.updater.pubkey`) and the stable
//! endpoint. The matching private key is the `TAURI_SIGNING_PRIVATE_KEY`
//! secret and lives nowhere in the repository.
//!
//! Swift: `apps/macos/Steno/Services/UpdaterController.swift`,
//! `apps/macos/Steno/Services/UpdateChannels.swift`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use steno_services::updates::{
    CHECK_TIMED_OUT, CHECK_TIMEOUT, MANAGED_CHECK, UpdateSchedule, UpdateSource,
};
use tauri::{AppHandle, Manager, Url};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogBuilder, MessageDialogButtons, MessageDialogKind,
};
use tauri_plugin_updater::{Update, UpdaterExt};

/// The stable lane: the rolling `desktop-stable` release, which carries the
/// newest release's manifest.
pub const STABLE_ENDPOINT: &str =
    "https://github.com/NicolaiSchmid/steno/releases/download/desktop-stable/latest.json";
/// The pre-release lane: the rolling `desktop-beta` release, which carries
/// the manifest of the newest pre-release or release.
pub const BETA_ENDPOINT: &str =
    "https://github.com/NicolaiSchmid/steno/releases/download/desktop-beta/latest.json";

/// Whether a marketing version is a pre-release (`UpdateChannels.allowed`:
/// a hyphen means the beta lane).
pub fn is_prerelease(version: &str) -> bool {
    version.contains('-')
}

/// The manifests a build reads, in order.
pub fn endpoints(version: &str) -> Vec<Url> {
    let mut urls = Vec::with_capacity(2);
    if is_prerelease(version) {
        urls.push(Url::parse(BETA_ENDPOINT).expect("a valid beta endpoint"));
    }
    urls.push(Url::parse(STABLE_ENDPOINT).expect("a valid stable endpoint"));
    urls
}

/// The plugin; [`ShellUpdates`]'s `UpdateSource::check` supplies the lanes
/// per build and `tauri.conf.json` the public key.
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry, tauri_plugin_updater::Config> {
    tauri_plugin_updater::Builder::new().build()
}

/// The update source the schedule drives, kept as Tauri managed state:
/// the update the last check found, which the schedule's download and
/// install name by version. The schedule keeps the downloaded package.
pub struct ShellUpdates {
    app: AppHandle,
    found: Mutex<Option<Update>>,
    /// One install at a time ([`OneInstall`]).
    installing: AtomicBool,
}

impl ShellUpdates {
    pub fn new(app: AppHandle) -> Self {
        ShellUpdates {
            app,
            found: Mutex::new(None),
            installing: AtomicBool::new(false),
        }
    }

    /// The found update when it is `version`; the error says why not.
    fn found(&self, version: &str) -> Result<Update, String> {
        lock(&self.found)
            .clone()
            .filter(|update| update.version == version)
            .ok_or_else(|| format!("Steno {version} is no longer the update on offer."))
    }
}

/// An install under way: while it lives a second one returns at once, so
/// two dialogs answered yes do not write two packages.
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

#[async_trait::async_trait]
impl UpdateSource for ShellUpdates {
    /// Asks the lanes. The update itself, when there is one, is kept for
    /// the download and the install.
    async fn check(&self) -> Result<Option<String>, String> {
        let version = self.app.package_info().version.to_string();
        let handle = self.app.clone();
        let update = self
            .app
            .updater_builder()
            .endpoints(endpoints(&version))
            .map_err(|error| error.to_string())?
            // Windows: the installer ends the process itself.
            .on_before_exit(move || crate::shut_down_before_exit(&handle))
            .build()
            .map_err(|error| error.to_string())?
            .check()
            .await
            .map_err(|error| error.to_string())?;
        let version = update.as_ref().map(|update| update.version.clone());
        *lock(&self.found) = update;
        Ok(version)
    }

    async fn download(&self, version: &str) -> Result<Vec<u8>, String> {
        self.found(version)?
            .download(|_, _| {}, || {})
            .await
            .map_err(|error| error.to_string())
    }

    /// Installs `version` (from `package` when the schedule kept one, else
    /// downloading it now), runs the shutdown and relaunches; a second
    /// install while one runs returns at once, and a version the last check
    /// no longer found is refused with a message.
    /// The relaunch bypasses the exit request, so the shutdown runs first
    /// (`shut_down_before_exit`), as Sparkle's relaunch went through
    /// `applicationShouldTerminate`; on Windows the installer's own exit
    /// runs it (`UpdateSource::check`). An install that fails after that
    /// shutdown ran (Windows: the installer did not launch) ends the app
    /// once its message is closed: the recorder and the pipeline start
    /// nothing after a shutdown, and the next Quit would run none.
    async fn install_and_relaunch(
        &self,
        version: &str,
        package: Option<Vec<u8>>,
    ) -> Result<(), String> {
        let app = &self.app;
        let Some(_one) = OneInstall::start(&self.installing) else {
            return Ok(());
        };
        let installed = async {
            let update = self.found(version)?;
            match package {
                Some(bytes) => update.install(bytes),
                None => update.download_and_install(|_, _| {}, || {}).await,
            }
            .map_err(|error| error.to_string())
        }
        .await;
        match installed {
            Ok(()) => {
                let handle = app.clone();
                let _ = tauri::async_runtime::spawn_blocking(move || {
                    crate::shut_down_before_exit(&handle);
                })
                .await;
                app.restart()
            }
            Err(error) => {
                let shut_down = app.state::<steno_services::app::ExitGate>().released();
                let handle = app.clone();
                dialog(
                    app,
                    MessageDialogKind::Error,
                    format!("The update could not be installed: {error}"),
                )
                .show(move |_| {
                    if shut_down {
                        handle.exit(0);
                    }
                });
                Err(error)
            }
        }
    }

    fn announce(&self, version: &str) {
        let (app, version) = (self.app.clone(), version.to_owned());
        tauri::async_runtime::spawn(async move { offer(&app, &version).await });
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The shell's update source.
fn source(app: &AppHandle) -> Arc<ShellUpdates> {
    app.state::<Arc<ShellUpdates>>().inner().clone()
}

/// The schedule that records each check, when the host runs one (not for
/// the fixture host, nor in a smoke run).
fn schedule(app: &AppHandle) -> Option<Arc<UpdateSchedule>> {
    app.try_state::<crate::host::Host>()?.updates()
}

/// The user's check of the lanes; through the schedule when there is one,
/// so the General section shows the outcome and the time and the schedule
/// does not announce the same version again. Either way it gives up after
/// [`CHECK_TIMEOUT`].
async fn check_on_request(app: &AppHandle) -> Result<Option<String>, String> {
    match schedule(app) {
        Some(schedule) => schedule.check_on_request().await,
        None => tokio::time::timeout(CHECK_TIMEOUT, source(app).check())
            .await
            .unwrap_or_else(|_| Err(CHECK_TIMED_OUT.to_owned())),
    }
}

/// A message from the updater, one button unless the caller adds more.
fn dialog(
    app: &AppHandle,
    kind: MessageDialogKind,
    message: impl Into<String>,
) -> MessageDialogBuilder<tauri::Wry> {
    app.dialog().message(message).title("Steno").kind(kind)
}

/// Shows a two-button question and waits for the answer: true for `yes`;
/// `no`, or a dialog closed without an answer, is false.
async fn ask(
    app: &AppHandle,
    kind: MessageDialogKind,
    message: String,
    yes: &str,
    no: &str,
) -> bool {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    dialog(app, kind, message)
        .buttons(MessageDialogButtons::OkCancelCustom(yes.into(), no.into()))
        .show(move |agreed| {
            let _ = sender.send(agreed);
        });
    receiver.await == Ok(true)
}

/// Whether a recording is starting, running or stopping now: the host's
/// recorder when the host runs the schedule, else (the fixture host, a
/// smoke run) the state the shell follows.
fn recording_under_way(app: &AppHandle) -> bool {
    match schedule(app) {
        Some(schedule) => schedule.recording_under_way(),
        None => steno_services::updates::stops_a_recording(crate::panels::recording(app)),
    }
}

/// The tray's Check for Updates: checks, then asks before installing,
/// as Sparkle's standard driver does. On a packaged install it says who
/// delivers the updates instead.
pub async fn check_and_offer(app: &AppHandle) {
    match check_on_request(app).await {
        Ok(Some(version)) => offer(app, &version).await,
        Ok(None) => {
            dialog(app, MessageDialogKind::Info, "Steno is up to date.").show(|_| {});
        }
        Err(message) if message == MANAGED_CHECK => {
            dialog(app, MessageDialogKind::Info, message).show(|_| {});
        }
        Err(message) => {
            dialog(
                app,
                MessageDialogKind::Error,
                format!("The update check failed: {message}"),
            )
            .show(|_| {});
        }
    }
}

/// Asks whether to install `version` now and, when the user agrees,
/// installs and relaunches: after the tray's check, and when the schedule
/// found an update it does not install by itself (Sparkle's update alert).
/// The dialog may have been open since before a recording started, so when
/// the recorder is no longer idle by the yes it asks again first
/// ("Installing stops and saves the recording in progress."); "Not Now",
/// or closing that dialog, leaves the version for the schedule to announce
/// once the recording has ended.
async fn offer(app: &AppHandle, version: &str) {
    const INSTALL: &str = "Install and Relaunch";
    let message = format!("Steno {version} is available. Install it and relaunch?");
    if !ask(app, MessageDialogKind::Info, message, INSTALL, "Later").await {
        return;
    }
    if recording_under_way(app) {
        let message = "Installing stops and saves the recording in progress.".to_owned();
        if !ask(app, MessageDialogKind::Warning, message, INSTALL, "Not Now").await {
            if let Some(schedule) = schedule(app) {
                schedule.announce_again(version);
            }
            return;
        }
    }
    match schedule(app) {
        Some(schedule) => schedule.install_on_request(version).await,
        None => {
            let _ = source(app).install_and_relaunch(version, None).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hyphen_means_the_beta_lane() {
        assert!(is_prerelease("0.11.0-rc.1"));
        assert!(is_prerelease("1.0.0-beta"));
        assert!(!is_prerelease("0.11.0"));
        assert!(!is_prerelease("1.0.0"));
    }

    #[test]
    fn a_prerelease_reads_beta_then_stable_and_a_release_stable_only() {
        let beta: Vec<String> = endpoints("0.11.0-rc.1")
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(beta, [BETA_ENDPOINT, STABLE_ENDPOINT]);
        let stable: Vec<String> = endpoints("0.11.0").into_iter().map(String::from).collect();
        assert_eq!(stable, [STABLE_ENDPOINT]);
    }

    #[test]
    fn the_endpoints_are_https_on_github() {
        for url in endpoints("0.11.0-rc.1") {
            assert_eq!(url.scheme(), "https");
            assert_eq!(url.host_str(), Some("github.com"));
            assert!(url.path().ends_with("/latest.json"));
        }
    }

    /// While one install runs a second does not start; once it ends, the
    /// next may.
    #[test]
    fn one_install_at_a_time() {
        let installing = AtomicBool::new(false);
        let first = OneInstall::start(&installing);
        assert!(first.is_some());
        assert!(OneInstall::start(&installing).is_none());
        drop(first);
        assert!(OneInstall::start(&installing).is_some());
    }

    #[test]
    fn the_configured_endpoint_is_the_stable_lane() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        assert_eq!(
            config["plugins"]["updater"]["endpoints"],
            serde_json::json!([STABLE_ENDPOINT])
        );
    }
}
