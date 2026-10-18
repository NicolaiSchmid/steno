//! Updates over `tauri-plugin-updater`: a signed manifest per lane on a
//! rolling GitHub release, checked on request and daily by the update
//! schedule (`steno_services::updates`) through [`ShellUpdates`]. A
//! pre-release build reads the beta lane first, then stable; a stable
//! build reads stable only. On Windows the installer runs under a watcher
//! (`windows_setup`) that restarts Steno when the install does not happen.

use std::sync::{Arc, Mutex, PoisonError};

use steno_services::updates::{
    Busy, CHECK_TIMED_OUT, CHECK_TIMEOUT, Installer, MANAGED_CHECK, Question, UpdateSchedule,
    UpdateSource,
};
use tauri::utils::config::BundleType;
use tauri::{AppHandle, Manager, Url};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogBuilder, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};
use tauri_plugin_updater::{Update, UpdaterExt};

#[cfg(any(windows, test))]
mod windows_setup;

/// The stable lane's manifest, on the rolling `desktop-stable` release.
pub const STABLE_ENDPOINT: &str =
    "https://github.com/NicolaiSchmid/steno/releases/download/desktop-stable/latest.json";
/// The beta lane's manifest, on the rolling `desktop-beta` release: the
/// newest pre-release or release.
pub const BETA_ENDPOINT: &str =
    "https://github.com/NicolaiSchmid/steno/releases/download/desktop-beta/latest.json";

const INSTALL: &str = "Install and Relaunch";
/// The install button while a recording or a processing job runs.
const INSTALL_AFTER: &str = "Install After It Ends";

/// Whether a marketing version is a pre-release (it has a hyphen).
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

/// The plugin; [`ShellUpdates`] supplies the lanes per build.
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry, tauri_plugin_updater::Config> {
    tauri_plugin_updater::Builder::new().build()
}

/// The update source the schedule drives, kept as Tauri managed state:
/// the update the last check found, and the dialogs.
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

    fn found(&self, version: &str) -> Result<Update, String> {
        on_offer(lock(&self.found).clone(), version, |update| &update.version)
    }
}

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
    /// Asks the lanes and keeps the update found for download and install.
    async fn check(&self) -> Result<Option<String>, String> {
        let version = self.app.package_info().version.to_string();
        let update = self
            .app
            .updater_builder()
            .endpoints(endpoints(&version))
            .map_err(|error| error.to_string())?
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

    /// Installs on a blocking thread, since the updater may wait on a
    /// password prompt.
    async fn install(&self, version: &str, package: Vec<u8>) -> Result<(), String> {
        let (app, update) = (self.app.clone(), self.found(version)?);
        tauri::async_runtime::spawn_blocking(move || install_update(&app, &update, &package))
            .await
            .map_err(|error| error.to_string())?
    }

    fn installer(&self) -> Installer {
        this_installer()
    }

    /// Runs the shutdown first; the restart's own exit request then finds
    /// the exit gate released (`main::exit_request`).
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

    /// An install that failed after its own shutdown ran (Windows) restarts
    /// the app at once into the running version, since nothing starts
    /// after a shutdown and nobody may be there to close a message. One
    /// that fails while the app quits is only logged.
    fn tell_install_failed(&self, message: &str) {
        if self.app.state::<steno_services::app::ExitGate>().released() {
            tracing::warn!(%message, "the update failed while Steno quits");
            steno_services::flush_logs();
            return;
        }
        app_dialog(
            &self.app,
            MessageDialogKind::Error,
            format!("The update could not be installed: {message}"),
        )
        .show(|_| {});
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

/// Installs `package`, the verified download of `update`, through the
/// plugin.
#[cfg(not(windows))]
fn install_update(_app: &AppHandle, update: &Update, package: &[u8]) -> Result<(), String> {
    update.install(package).map_err(|error| error.to_string())
}

/// Windows: writes the installer under the app's local data folder, runs
/// the shutdown, starts the watcher (`windows_setup`), waits up to
/// [`WATCHER_START_LIMIT`] for it to report, releases the single-instance
/// lock and ends the process. Returns only when it failed before the
/// shutdown; a watcher failing after the shutdown restarts the app.
///
/// [`WATCHER_START_LIMIT`]: windows_setup::WATCHER_START_LIMIT
#[cfg(windows)]
fn install_update(app: &AppHandle, update: &Update, package: &[u8]) -> Result<(), String> {
    use windows_setup::{
        STARTED_FILE, Setup, WATCHER_START_LIMIT, empty_update_folder, system32, wait_for_start,
        watcher_command,
    };

    let text = |error: std::io::Error| error.to_string();
    let setup = Setup::of(package).ok_or("The update is not a Windows installer.")?;
    let data = app
        .path()
        .app_local_data_dir()
        .map_err(|error| error.to_string())?;
    let folder = data.join("update");
    let installer = folder.join(setup.file_name(&update.version));
    let started = folder.join(STARTED_FILE);
    empty_update_folder(&folder)?;
    std::fs::write(&installer, package).map_err(text)?;
    let relaunch = std::env::current_exe().map_err(text)?;
    let mut watcher = watcher_command(
        setup,
        &installer,
        &system32().join("msiexec.exe"),
        &relaunch,
        &started,
        &data,
    );
    crate::shut_down_before_exit(app);
    let ran = watcher
        .spawn()
        .map_err(text)
        .and_then(|mut watcher| wait_for_start(&mut watcher, &started, WATCHER_START_LIMIT));
    match ran {
        Ok(()) => {
            // Freed first: an installer that fails at once has the watcher
            // start this version within milliseconds, which would otherwise
            // hand itself to this ending process. `exit` skips the usual
            // release on `RunEvent::Exit`.
            tauri_plugin_single_instance::destroy(app);
            std::process::exit(0)
        }
        Err(message) => {
            tracing::warn!(%message, "the update failed after the shutdown; Steno restarts");
            steno_services::flush_logs();
            app.restart()
        }
    }
}

fn this_installer() -> Installer {
    installer_for(
        cfg!(windows),
        tauri::utils::platform::bundle_type().as_ref(),
        bundle_is_writable,
    )
}

/// How the updater installs for the bundle the binary came in. On Windows
/// the install ends the process; a `.deb` or `.rpm` asks for a password;
/// a macOS bundle asks for one only when the user cannot write it or its
/// folder (`writable`, asked only then); anything else is replaced in
/// place.
fn installer_for(
    windows: bool,
    bundle: Option<&BundleType>,
    writable: impl FnOnce() -> bool,
) -> Installer {
    if windows {
        return match bundle {
            Some(BundleType::Msi) => Installer::EndsTheAppThenAsks,
            _ => Installer::EndsTheApp,
        };
    }
    match bundle {
        Some(BundleType::Deb | BundleType::Rpm) => Installer::AsksForAPassword,
        Some(BundleType::App) if !writable() => Installer::AsksForAPassword,
        _ => Installer::InPlace,
    }
}

#[cfg(target_os = "macos")]
fn bundle_is_writable() -> bool {
    std::env::current_exe().is_ok_and(|executable| bundle_is_writable_at(&executable))
}

/// Whether the user can write the bundle of `executable` and its folder,
/// which the updater renames the bundle out of.
#[cfg(target_os = "macos")]
fn bundle_is_writable_at(executable: &std::path::Path) -> bool {
    let writable =
        |path: &std::path::Path| rustix::fs::access(path, rustix::fs::Access::WRITE_OK).is_ok();
    let Some(bundle) = executable.ancestors().nth(3) else {
        return false;
    };
    writable(bundle) && bundle.parent().is_some_and(writable)
}

#[cfg(not(target_os = "macos"))]
fn bundle_is_writable() -> bool {
    true
}

/// A question's dialog: kind, text and buttons, the default first, and the
/// label of the one that installs.
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
            // Neither button stops the recording or the processing.
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
            Question::InstallNow(version) => Dialog {
                kind: MessageDialogKind::Info,
                message: format!("Steno {version} is ready to install. Install it and relaunch now?"),
                buttons: MessageDialogButtons::OkCancelCustom(INSTALL.into(), "Later".into()),
                install: INSTALL,
            },
        }
    }
}

/// Tells the user Steno relaunches once the recording or processing ends.
fn relaunch_waits_message(version: &str, busy: Busy) -> String {
    match busy {
        Busy::Recording => format!(
            "Steno {version} is installed. Steno relaunches once the recording is saved and processed."
        ),
        Busy::Processing => {
            format!("Steno {version} is installed. Steno relaunches once the meeting is processed.")
        }
    }
}

/// Whether the button pressed is the dialog's install button.
fn installs(result: &MessageDialogResult, install: &str) -> bool {
    matches!(result, MessageDialogResult::Custom(label) if label == install)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn source(app: &AppHandle) -> Arc<ShellUpdates> {
    app.state::<Arc<ShellUpdates>>().inner().clone()
}

/// The update schedule, when the host runs one (not for the fixture host,
/// nor in a smoke run).
fn schedule(app: &AppHandle) -> Option<Arc<UpdateSchedule>> {
    app.try_state::<crate::host::Host>()?.updates()
}

/// The user's check of the lanes, through the schedule when there is one
/// so the General section shows the outcome; gives up after
/// [`CHECK_TIMEOUT`].
async fn check_on_request(app: &AppHandle) -> Result<Option<String>, String> {
    match schedule(app) {
        Some(schedule) => schedule.check_on_request().await,
        None => tokio::time::timeout(CHECK_TIMEOUT, source(app).check())
            .await
            .unwrap_or_else(|_| Err(CHECK_TIMED_OUT.to_owned())),
    }
}

fn app_dialog(
    app: &AppHandle,
    kind: MessageDialogKind,
    message: impl Into<String>,
) -> MessageDialogBuilder<tauri::Wry> {
    app.dialog().message(message).title("Steno").kind(kind)
}

/// The tray's Check for Updates: checks, then offers to install. On a
/// packaged install it says who delivers the updates instead; without a
/// schedule it only says what it found.
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

    /// The default button installs; while busy it installs once that ends,
    /// and the other button is "Not Now".
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

    /// Asked again before an administrator's installer, "Later" is the
    /// other button.
    #[test]
    fn asked_again_the_default_installs_now() {
        let dialog = Dialog::of(Question::InstallNow("0.12.0"));
        assert!(dialog.message.contains("0.12.0"), "{}", dialog.message);
        assert!(dialog.message.contains("now"), "{}", dialog.message);
        let MessageDialogButtons::OkCancelCustom(default, other) = dialog.buttons else {
            panic!("{:?}", dialog.buttons);
        };
        assert_eq!((default.as_str(), other.as_str()), (INSTALL, "Later"));
        assert_eq!(dialog.install, INSTALL);
    }

    /// The installer for every platform and bundle; writability is asked
    /// only of the macOS bundle.
    #[test]
    fn the_installer_follows_the_platform_and_the_bundle() {
        use BundleType::{App, AppImage, Deb, Dmg, Msi, Nsis, Rpm};
        let never = || -> bool { panic!("writability asked") };
        assert_eq!(
            installer_for(true, Some(&Msi), never),
            Installer::EndsTheAppThenAsks
        );
        for bundle in [Some(&Nsis), None] {
            assert_eq!(installer_for(true, bundle, never), Installer::EndsTheApp);
        }
        for bundle in [Deb, Rpm] {
            assert_eq!(
                installer_for(false, Some(&bundle), never),
                Installer::AsksForAPassword
            );
        }
        for bundle in [Some(&AppImage), Some(&Dmg), None] {
            assert_eq!(installer_for(false, bundle, never), Installer::InPlace);
        }
        assert_eq!(
            installer_for(false, Some(&App), || true),
            Installer::InPlace
        );
        assert_eq!(
            installer_for(false, Some(&App), || false),
            Installer::AsksForAPassword
        );
    }

    /// Only the dialog's own install button installs.
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

    /// A test binary has no bundle.
    #[test]
    fn an_unbundled_build_installs_as_its_platform_does() {
        let expected = if cfg!(windows) {
            Installer::EndsTheApp
        } else {
            Installer::InPlace
        };
        assert_eq!(this_installer(), expected);
    }

    /// An unwritable bundle or folder asks for a password. Skipped as root.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_unwritable_bundle_or_folder_asks_for_a_password() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &std::path::Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        let folder = tempfile::tempdir().unwrap();
        let bundle = folder.path().join("Steno.app");
        let executable = bundle.join("Contents/MacOS/steno-desktop");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, b"").unwrap();
        assert!(bundle_is_writable_at(&executable));
        assert!(!bundle_is_writable_at(std::path::Path::new(
            "steno-desktop"
        )));
        mode(&bundle, 0o555);
        if std::fs::write(bundle.join("probe"), b"").is_ok() {
            mode(&bundle, 0o755);
            eprintln!("skipped: root writes a read-only folder");
            return;
        }
        assert!(!bundle_is_writable_at(&executable), "bundle read-only");
        mode(&bundle, 0o755);
        mode(folder.path(), 0o555);
        assert!(!bundle_is_writable_at(&executable), "folder read-only");
        mode(folder.path(), 0o755);
    }

    /// Download and install accept only the update the last check found.
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
