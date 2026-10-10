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

use std::sync::{Arc, Mutex, PoisonError};

use steno_services::updates::{
    Busy, CHECK_TIMED_OUT, CHECK_TIMEOUT, Installer, MANAGED_CHECK, Question, UpdateSchedule,
    UpdateSource,
};
use tauri::{AppHandle, Manager, Url};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogBuilder, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
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

/// The update alert's install button.
const INSTALL: &str = "Install and Relaunch";
/// The install button while a recording or a processing job runs.
const INSTALL_AFTER: &str = "Install After It Ends";

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
/// install name by version, and the dialogs. The schedule decides the
/// order and keeps the downloaded package.
pub struct ShellUpdates {
    app: AppHandle,
    found: Mutex<Option<Update>>,
}

impl ShellUpdates {
    pub fn new(app: AppHandle) -> Self {
        ShellUpdates {
            app,
            found: Mutex::new(None),
        }
    }

    /// The found update when it is `version`; the error says why not.
    fn found(&self, version: &str) -> Result<Update, String> {
        on_offer(lock(&self.found).clone(), version, |update| &update.version)
    }
}

/// `found` when it is `version`; the error says why not.
fn on_offer<T>(
    found: Option<T>,
    version: &str,
    version_of: impl Fn(&T) -> &str,
) -> Result<T, String> {
    found
        .filter(|update| version_of(update) == version)
        .ok_or_else(|| format!("Steno {version} is no longer the update on offer."))
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

    /// Writes the package on a blocking thread, since the updater may wait
    /// on a password prompt; on Windows the installer's own exit runs the
    /// shutdown (`UpdateSource::check`).
    async fn install(&self, version: &str, package: Vec<u8>) -> Result<(), String> {
        let update = self.found(version)?;
        tauri::async_runtime::spawn_blocking(move || update.install(package))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())
    }

    /// The updater's choice by the bundle the binary came in: on Windows
    /// it starts the installer and ends the process; a `.deb` or an `.rpm`
    /// is installed through pkexec, then a zenity or kdialog password
    /// dialog, then `sudo`, each of which waits for an answer; anything
    /// else (an `AppImage`, the macOS bundle) is replaced in place.
    fn installer(&self) -> Installer {
        use tauri::utils::config::BundleType;
        if cfg!(windows) {
            return Installer::EndsTheApp;
        }
        match tauri::utils::platform::bundle_type() {
            Some(BundleType::Deb | BundleType::Rpm) => Installer::AsksForAPassword,
            _ => Installer::InPlace,
        }
    }

    /// Runs the shutdown first (`shut_down_for_relaunch`), as Sparkle's
    /// relaunch went through `applicationShouldTerminate`. The restart's
    /// own exit request (`tauri::RESTART_EXIT_CODE`) then finds the exit
    /// gate released and goes through (`main::exit_request`).
    async fn relaunch(&self) {
        let handle = self.app.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || {
            crate::shut_down_for_relaunch(&handle);
        })
        .await;
        self.app.restart()
    }

    async fn ask(&self, question: Question<'_>) -> bool {
        let dialog = Dialog::of(question);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        app_dialog(&self.app, dialog.kind, dialog.message)
            .buttons(dialog.buttons)
            .show_with_result(move |result| {
                let _ = sender.send(result);
            });
        receiver
            .await
            .is_ok_and(|result| installs(&result, dialog.install))
    }

    /// An install that fails after the shutdown ran (Windows: the
    /// installer did not launch) ends the app once its message is closed:
    /// the recorder and the pipeline start nothing after a shutdown, and
    /// the next Quit would run none.
    fn tell_install_failed(&self, message: &str) {
        let shut_down = self.app.state::<steno_services::app::ExitGate>().released();
        let handle = self.app.clone();
        app_dialog(
            &self.app,
            MessageDialogKind::Error,
            format!("The update could not be installed: {message}"),
        )
        .show(move |_| {
            if shut_down {
                handle.exit(0);
            }
        });
    }

    fn tell_relaunch_waits(&self, version: &str, busy: Busy) {
        app_dialog(
            &self.app,
            MessageDialogKind::Info,
            relaunch_waits_message(version, busy),
        )
        .show(|_| {});
    }

    fn announce(&self, version: &str) {
        let (app, version) = (self.app.clone(), version.to_owned());
        tauri::async_runtime::spawn(async move {
            if let Some(schedule) = schedule(&app) {
                schedule.offer(&version).await;
            }
        });
    }
}

/// A question's dialog: its kind, its text and its buttons, the default
/// first (macOS and Windows press the first button on Return, and GTK
/// focuses it), and the label of the one that installs.
struct Dialog {
    kind: MessageDialogKind,
    message: String,
    buttons: MessageDialogButtons,
    install: &'static str,
}

impl Dialog {
    fn of(question: Question<'_>) -> Self {
        match question {
            Question::Install(version) => Dialog {
                kind: MessageDialogKind::Info,
                message: format!("Steno {version} is available. Install it and relaunch?"),
                buttons: MessageDialogButtons::OkCancelCustom(INSTALL.into(), "Later".into()),
                install: INSTALL,
            },
            // Installing after it ends is the default: neither button
            // stops the recording or the processing (stable plan P25).
            Question::AfterItEnds(busy) => Dialog {
                kind: MessageDialogKind::Info,
                message: match busy {
                    Busy::Recording => "Steno is recording. Install the update and relaunch once the recording is saved and processed?",
                    Busy::Processing => "Steno is still processing a meeting. Install the update and relaunch once it is done?",
                }
                .to_owned(),
                buttons: MessageDialogButtons::OkCancelCustom(
                    INSTALL_AFTER.into(),
                    "Not Now".into(),
                ),
                install: INSTALL_AFTER,
            },
        }
    }
}

/// What the user is told when an update installed while a recording or a
/// processing job began: Steno relaunches into it once that is done.
fn relaunch_waits_message(version: &str, busy: Busy) -> String {
    match busy {
        Busy::Recording => format!(
            "Steno {version} is installed. Steno relaunches into it once the recording is saved and processed."
        ),
        Busy::Processing => format!(
            "Steno {version} is installed. Steno relaunches into it once the meeting is processed."
        ),
    }
}

/// Whether the button pressed installs: only the label `install`, the
/// dialog's install button; the other button, Escape and closing the
/// dialog do not.
fn installs(result: &MessageDialogResult, install: &str) -> bool {
    matches!(result, MessageDialogResult::Custom(label) if label == install)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The shell's update source.
fn source(app: &AppHandle) -> Arc<ShellUpdates> {
    app.state::<Arc<ShellUpdates>>().inner().clone()
}

/// The schedule that records each check and installs, when the host runs
/// one (not for the fixture host, nor in a smoke run).
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
fn app_dialog(
    app: &AppHandle,
    kind: MessageDialogKind,
    message: impl Into<String>,
) -> MessageDialogBuilder<tauri::Wry> {
    app.dialog().message(message).title("Steno").kind(kind)
}

/// The tray's Check for Updates: checks, then offers to install
/// (`UpdateSchedule::offer`), as Sparkle's standard driver does. On a
/// packaged install it says who delivers the updates instead. Without a
/// schedule (the fixture host, a smoke run) it only says what it found.
pub async fn check_and_offer(app: &AppHandle) {
    match check_on_request(app).await {
        Ok(Some(version)) => {
            if let Some(schedule) = schedule(app) {
                schedule.offer(&version).await;
            } else {
                let message =
                    format!("Steno {version} is available. This build does not install it.");
                app_dialog(app, MessageDialogKind::Info, message).show(|_| {});
            }
        }
        Ok(None) => {
            app_dialog(app, MessageDialogKind::Info, "Steno is up to date.").show(|_| {});
        }
        Err(message) if message == MANAGED_CHECK => {
            app_dialog(app, MessageDialogKind::Info, message).show(|_| {});
        }
        Err(message) => {
            app_dialog(
                app,
                MessageDialogKind::Error,
                format!("The update check failed: {message}"),
            )
            .show(|_| {});
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

    /// The update alert's default installs, as Sparkle's does; while the
    /// app is busy the default installs once that ends, and the other
    /// button is "Not Now". The busy dialog names what it waits for in
    /// plain words.
    #[test]
    fn the_busy_dialogs_default_installs_once_it_ends() {
        let buttons = |question| {
            let dialog = Dialog::of(question);
            match dialog.buttons {
                MessageDialogButtons::OkCancelCustom(first, second) => {
                    (first, second, dialog.install, dialog.message)
                }
                other => panic!("{other:?}"),
            }
        };
        let (default, _, install, _) = buttons(Question::Install("0.12.0"));
        assert_eq!((default.as_str(), install), (INSTALL, INSTALL));
        for (busy, named) in [
            (Busy::Recording, "recording"),
            (Busy::Processing, "processing"),
        ] {
            let (default, other, install, message) = buttons(Question::AfterItEnds(busy));
            assert_eq!(default, INSTALL_AFTER);
            assert_eq!(install, INSTALL_AFTER);
            assert_eq!(other, "Not Now");
            assert!(message.contains(named), "{message}");
            assert!(installs(&MessageDialogResult::Custom(default), install));
            assert!(!installs(&MessageDialogResult::Custom(other), install));
        }
    }

    /// The relaunch that waits names the version and what it waits for.
    #[test]
    fn the_relaunch_that_waits_says_what_for() {
        let recording = relaunch_waits_message("0.12.0", Busy::Recording);
        assert!(
            recording.starts_with("Steno 0.12.0 is installed."),
            "{recording}"
        );
        assert!(recording.contains("recording is saved"), "{recording}");
        let processing = relaunch_waits_message("0.12.0", Busy::Processing);
        assert!(processing.contains("processed"), "{processing}");
    }

    /// Only the install button installs: Escape, closing the dialog and
    /// any other result do not, nor the other dialog's install button.
    #[test]
    fn only_the_install_button_installs() {
        for result in [
            MessageDialogResult::Cancel,
            MessageDialogResult::Ok,
            MessageDialogResult::Yes,
            MessageDialogResult::No,
            MessageDialogResult::Custom("Later".into()),
            MessageDialogResult::Custom(INSTALL_AFTER.into()),
        ] {
            assert!(!installs(&result, INSTALL), "{result:?}");
        }
        assert!(!installs(
            &MessageDialogResult::Custom(INSTALL.into()),
            INSTALL_AFTER
        ));
    }

    /// The download and the install name the update the last check found,
    /// and only that one.
    #[test]
    fn only_the_version_on_offer_installs() {
        let found = |version: &str| Some(version.to_owned());
        assert_eq!(
            on_offer(found("0.12.0"), "0.12.0", String::as_str),
            Ok("0.12.0".to_owned())
        );
        let refused = Err("Steno 0.12.0 is no longer the update on offer.".to_owned());
        assert_eq!(on_offer(found("0.13.0"), "0.12.0", String::as_str), refused);
        assert_eq!(on_offer(None, "0.12.0", String::as_str), refused);
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
