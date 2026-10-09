//! Updates over `tauri-plugin-updater`: a signed manifest per lane on the
//! GitHub release, checked on request from the tray or from Settings
//! (`updates.check`), and daily by the update schedule
//! (`steno_services::updates`), which drives [`ShellUpdates`], records each
//! check's outcome and time for the General section, and installs by
//! itself only through its install gate. The lane follows the installed version, as Sparkle's
//! channel does in the Swift app: a pre-release build ("0.11.0-rc.1") reads
//! the beta lane's manifest first and falls back to the stable one; a
//! stable build reads the stable manifest only. No setting.
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
//! Swift: `UpdaterController.swift`, `UpdateChannels.swift`.

use std::sync::{Arc, Mutex, PoisonError};

use steno_services::updates::{UpdateSchedule, UpdateSource};
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

/// The plugin; `check` supplies the lanes per build and `tauri.conf.json`
/// the public key.
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry, tauri_plugin_updater::Config> {
    tauri_plugin_updater::Builder::new().build()
}

/// The update source the schedule drives, managed state: the update the
/// last check found and, once downloaded, its verified bytes, which an
/// install the user agrees to uses instead of downloading again.
pub struct ShellUpdates {
    app: AppHandle,
    found: Mutex<Option<Update>>,
    /// The version and the bytes [`UpdateSource::download`] kept.
    downloaded: Mutex<Option<(String, Vec<u8>)>>,
}

impl ShellUpdates {
    pub fn new(app: AppHandle) -> Self {
        ShellUpdates {
            app,
            found: Mutex::new(None),
            downloaded: Mutex::new(None),
        }
    }

    /// Asks the lanes. The update itself, when there is one, is kept for
    /// the download and the install.
    async fn ask_the_lanes(&self) -> Result<Option<Update>, String> {
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
        lock(&self.found).clone_from(&update);
        Ok(update)
    }

    /// Installs the found update (from the kept bytes when they are its
    /// version, else downloading it now), runs the shutdown and relaunches.
    /// The relaunch bypasses the exit request, so the shutdown runs first
    /// (`shut_down_before_exit`), as Sparkle's relaunch went through
    /// `applicationShouldTerminate`; on Windows the installer's own exit
    /// runs it (`ask_the_lanes`). An install that fails after that shutdown
    /// ran (Windows: the installer did not launch) ends the app once its
    /// message is closed: the recorder and the pipeline start nothing after
    /// a shutdown, and the next Quit would run none.
    async fn install(&self) -> Result<(), String> {
        let app = &self.app;
        let Some(update) = lock(&self.found).clone() else {
            return Err("No update was found to install.".to_owned());
        };
        let kept = lock(&self.downloaded)
            .take_if(|(version, _)| *version == update.version)
            .map(|(_, bytes)| bytes);
        let installed = match kept {
            Some(bytes) => update.install(bytes),
            None => update.download_and_install(|_, _| {}, || {}).await,
        };
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
                Err(error.to_string())
            }
        }
    }
}

#[async_trait::async_trait]
impl UpdateSource for ShellUpdates {
    async fn check(&self) -> Result<Option<String>, String> {
        Ok(self.ask_the_lanes().await?.map(|update| update.version))
    }

    async fn download(&self) -> Result<(), String> {
        let Some(update) = lock(&self.found).clone() else {
            return Err("No update was found to download.".to_owned());
        };
        if lock(&self.downloaded)
            .as_ref()
            .is_some_and(|(version, _)| *version == update.version)
        {
            return Ok(());
        }
        let bytes = update
            .download(|_, _| {}, || {})
            .await
            .map_err(|error| error.to_string())?;
        *lock(&self.downloaded) = Some((update.version, bytes));
        Ok(())
    }

    async fn install_and_relaunch(&self) -> Result<(), String> {
        self.install().await
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

/// Checks the lanes; through the schedule when there is one, so the
/// General section shows the outcome and the time.
async fn check(app: &AppHandle) -> Result<Option<String>, String> {
    match schedule(app) {
        Some(schedule) => schedule.check_now().await,
        None => source(app).check().await,
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

/// The tray's Check for Updates: checks, then asks before installing,
/// as Sparkle's standard driver does.
pub async fn check_and_offer(app: &AppHandle) {
    match check(app).await {
        Ok(Some(version)) => offer(app, &version).await,
        Ok(None) => {
            dialog(app, MessageDialogKind::Info, "Steno is up to date.").show(|_| {});
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

/// Asks whether to install `version` now, and installs and relaunches when
/// the user agrees: after the tray's check, and when the schedule found an
/// update it does not install by itself (Sparkle's update alert).
async fn offer(app: &AppHandle, version: &str) {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    dialog(
        app,
        MessageDialogKind::Info,
        format!("Steno {version} is available. Install it and relaunch?"),
    )
    .buttons(MessageDialogButtons::OkCancelCustom(
        "Install and Relaunch".into(),
        "Later".into(),
    ))
    .show(move |agreed| {
        let _ = sender.send(agreed);
    });
    if receiver.await != Ok(true) {
        return;
    }
    if let Err(message) = source(app).install().await
        && let Some(schedule) = schedule(app)
    {
        schedule.install_failed(message);
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
