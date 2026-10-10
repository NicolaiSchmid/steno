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
//! On Windows the app runs the installer under a watcher that starts the
//! running version again when the installer does not install
//! (`windows_setup`), in place of the plugin's install, which left Steno
//! down after a declined consent prompt or a failed setup.
//!
//! Swift: `apps/macos/Steno/Services/UpdaterController.swift`,
//! `apps/macos/Steno/Services/UpdateChannels.swift`.

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

    /// Installs the package on a blocking thread ([`install_update`]),
    /// since the updater may wait on a password prompt.
    async fn install(&self, version: &str, package: Vec<u8>) -> Result<(), String> {
        let (app, update) = (self.app.clone(), self.found(version)?);
        tauri::async_runtime::spawn_blocking(move || install_update(&app, &update, &package))
            .await
            .map_err(|error| error.to_string())?
    }

    fn installer(&self) -> Installer {
        this_installer()
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

    /// An install that fails after the shutdown ran (Windows: the watcher
    /// did not start, or did not say in time that it runs) restarts the
    /// app at once, into the version that runs, with the failure in the
    /// log: the recorder and the pipeline start nothing after a shutdown,
    /// and on a scheduled install nobody may be there to close a message.
    /// The next check offers the update again.
    fn tell_install_failed(&self, message: &str) {
        if self.app.state::<steno_services::app::ExitGate>().released() {
            tracing::warn!(%message, "the update failed after the shutdown; Steno restarts");
            steno_services::flush_logs();
            self.app.restart();
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

/// Installs `package`, the verified download of `update`: through the
/// plugin, except on Windows.
#[cfg(not(windows))]
fn install_update(_app: &AppHandle, update: &Update, package: &[u8]) -> Result<(), String> {
    update.install(package).map_err(|error| error.to_string())
}

/// Windows: writes the installer under the app's local data folder
/// (`%LOCALAPPDATA%\uno.schmid.steno.desktop\update\`, which holds only
/// the last update's), runs the shutdown, starts the watcher
/// (`windows_setup`), waits up to [`WATCHER_START_LIMIT`] for it to say
/// that it runs, and ends the process; returns only when it failed. A
/// watcher that does not say so in time (a policy that turns off the
/// command prompt) is ended, and the app restarts
/// (`UpdateSource::tell_install_failed`); so does a job that forbids
/// breakaway, which makes the start fail. The watcher's current directory
/// is the local data folder, not the install folder the installer
/// replaces.
///
/// [`WATCHER_START_LIMIT`]: windows_setup::WATCHER_START_LIMIT
#[cfg(windows)]
fn install_update(app: &AppHandle, update: &Update, package: &[u8]) -> Result<(), String> {
    use windows_setup::{STARTED_FILE, Setup, WATCHER_START_LIMIT};

    let text = |error: std::io::Error| error.to_string();
    let setup = Setup::of(package).ok_or("The update is not a Windows installer.")?;
    let data = app
        .path()
        .app_local_data_dir()
        .map_err(|error| error.to_string())?;
    let folder = data.join("update");
    let installer = folder.join(setup.file_name(&update.version));
    // An earlier update's installer goes, and its start file with it.
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).map_err(text)?;
    std::fs::write(&installer, package).map_err(text)?;
    let relaunch = std::env::current_exe().map_err(text)?;
    let started = folder.join(STARTED_FILE);
    let mut watcher = windows_setup::watcher_command(
        setup,
        &installer,
        &windows_setup::system32().join("msiexec.exe"),
        &relaunch,
        &started,
        &data,
    );
    crate::shut_down_before_exit(app);
    let mut watcher = watcher.spawn().map_err(text)?;
    windows_setup::wait_for_start(&mut watcher, &started, WATCHER_START_LIMIT)?;
    std::process::exit(0)
}

/// [`installer_for`] this build's platform and bundle.
fn this_installer() -> Installer {
    installer_for(
        cfg!(windows),
        tauri::utils::platform::bundle_type().as_ref(),
        bundle_is_writable,
    )
}

/// How the updater installs on this platform (`windows` on Windows) for
/// the bundle the binary came in (`bundle`). On Windows the install ends
/// the process (`install_update`): the NSIS setup installs for the user
/// alone (`bundle.windows.nsis.installMode` in `tauri.conf.json`), and the
/// MSI for every user, after Windows' consent prompt. A `.deb` or an
/// `.rpm` is installed through pkexec, then a zenity or kdialog password
/// dialog, then `sudo`, each of which waits for an answer. The macOS
/// bundle is replaced in place, after an administrator's password when the
/// user cannot write it or its folder (`writable`, asked only then; the
/// prompt holds the app's windows until it is answered). Anything else (an
/// `AppImage`, or no bundle) is replaced in place.
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

/// Whether the user can write the running macOS bundle and its folder
/// ([`bundle_is_writable_at`]).
#[cfg(target_os = "macos")]
fn bundle_is_writable() -> bool {
    std::env::current_exe().is_ok_and(|executable| bundle_is_writable_at(&executable))
}

/// Whether the user can write the bundle of `executable`
/// (`Steno.app/Contents/MacOS/steno-desktop`) and its folder, which the
/// updater renames the bundle out of.
#[cfg(target_os = "macos")]
fn bundle_is_writable_at(executable: &std::path::Path) -> bool {
    let writable =
        |path: &std::path::Path| rustix::fs::access(path, rustix::fs::Access::WRITE_OK).is_ok();
    let Some(bundle) = executable.ancestors().nth(3) else {
        return false;
    };
    writable(bundle) && bundle.parent().is_some_and(writable)
}

/// No macOS bundle here.
#[cfg(not(target_os = "macos"))]
fn bundle_is_writable() -> bool {
    true
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
            Question::InstallNow(version) => Dialog {
                kind: MessageDialogKind::Info,
                message: format!("Steno {version} is ready to install. Install it and relaunch now?"),
                buttons: MessageDialogButtons::OkCancelCustom(INSTALL.into(), "Later".into()),
                install: INSTALL,
            },
        }
    }
}

/// What the user is told when an update installed while a recording or a
/// processing job began: Steno relaunches once that is done.
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

    /// Asked again before an installer that needs an administrator, the
    /// default installs now and the other button is "Later".
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

    /// Every platform and bundle: Windows ends the app, and its MSI asks
    /// for consent after; a `.deb` or an `.rpm` asks for a password, and so
    /// does a macOS bundle the user cannot write; the rest is replaced in
    /// place. Writability is asked only of the macOS bundle.
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

    /// This build's installer: a test binary comes in no bundle, so Windows
    /// ends the app and every other platform replaces it in place.
    #[test]
    fn an_unbundled_build_installs_as_its_platform_does() {
        let expected = if cfg!(windows) {
            Installer::EndsTheApp
        } else {
            Installer::InPlace
        };
        assert_eq!(this_installer(), expected);
    }

    /// A bundle the user cannot write, or one in a folder the user cannot
    /// write, asks for a password; one the user can write does not. Skipped
    /// as root, who can write either.
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
