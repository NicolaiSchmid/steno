//! Launch at login over `tauri-plugin-autostart`: a Launch Agent on macOS,
//! the `autostart` desktop entry on Linux, the Run registry key on Windows.
//! The Swift app registers itself with `SMAppService`, whose
//! `requiresApproval` state has no Launch Agent counterpart and never
//! occurs here. The status is the host's `LoginItemStatus`, and
//! `ShellLoginItem` is the host's `LoginItem` over this module (`WP6b`), so
//! the General section reads and switches the real registration.
//!
//! On Linux outside an `AppImage` the shell writes the `autostart` entry
//! itself, in the plugin's form and file, so that it names a path that
//! outlives an upgrade (`packaged::write_entry`) rather than the plugin's
//! `current_exe()`. When the system starts the app at login
//! (`STENO_LOGIN_ITEM=managed`, `packaged`), the status is `Managed` and
//! nothing here changes the registration; only an entry an earlier build
//! wrote goes, at launch, or at the exit while the app runs as the unit
//! made from it (`at_launch`, `at_exit`).
//!
//! Swift: `LoginItemController.swift`, `LoginItemStatus` in `AppProtocols.swift`.

use steno_core::protocols::BoundaryResult;
pub use steno_host::services::LoginItemStatus;
use tauri::AppHandle;
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use crate::bridge::{BridgeError, failed};
use crate::packaged;

/// The status from the plugin's answer; a registration the plugin could
/// not read is `NotFound`, its reason logged.
pub fn status_from_plugin(result: Result<bool, impl std::fmt::Display>) -> LoginItemStatus {
    match result {
        Ok(true) => LoginItemStatus::Enabled,
        Ok(false) => LoginItemStatus::NotRegistered,
        Err(error) => {
            tracing::debug!(%error, "the login item could not be read");
            LoginItemStatus::NotFound
        }
    }
}

/// The plugin, configured as the Swift app behaves: a Launch Agent (no
/// `AppleScript` prompt), no launch arguments.
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None)
}

/// The registration; `Managed` while the system starts the app at login.
pub fn status(app: &AppHandle) -> LoginItemStatus {
    status_unless_managed(packaged::login_item_is_managed(), || {
        app.autolaunch().is_enabled()
    })
}

/// `Managed` when `managed`, without asking the plugin; else the
/// plugin's answer (`status_from_plugin`).
fn status_unless_managed<E: std::fmt::Display>(
    managed: bool,
    plugin: impl FnOnce() -> Result<bool, E>,
) -> LoginItemStatus {
    if managed {
        return LoginItemStatus::Managed;
    }
    status_from_plugin(plugin())
}

/// Whether the tray's item may switch the login item: not one the system
/// manages, which shows checked and disabled.
pub fn switchable(status: LoginItemStatus) -> bool {
    status != LoginItemStatus::Managed
}

/// Registers or removes the login item; a plugin failure is `failed`,
/// which the page shows as it would any other refused command. Changes
/// nothing while the system manages the login item.
pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), BridgeError> {
    change_unless_managed(packaged::login_item_is_managed(), || {
        if enabled {
            enable(app)
        } else {
            app.autolaunch().disable()
        }
    })
}

/// `change` unless `managed`, its failure as `failed`.
fn change_unless_managed<E: std::fmt::Display>(
    managed: bool,
    change: impl FnOnce() -> Result<(), E>,
) -> Result<(), BridgeError> {
    if managed {
        tracing::debug!("launch at login is the system's; the login item stays as it is");
        return Ok(());
    }
    change().map_err(failed)
}

/// Writes the login item: on Linux outside an `AppImage` the entry that
/// names a stable path (`packaged::write_entry`), elsewhere the plugin's.
fn enable(app: &AppHandle) -> Result<(), tauri_plugin_autostart::Error> {
    #[cfg(target_os = "linux")]
    if writes_own_entry(tauri::Manager::env(app).appimage.as_deref()) {
        return Ok(packaged::write_entry(&app.package_info().name)?);
    }
    app.autolaunch().enable()
}

/// Whether the shell writes the Linux entry itself: outside an `AppImage`
/// (`appimage`, its `$APPIMAGE`), whose plugin entry already names a
/// stable path.
#[cfg(target_os = "linux")]
fn writes_own_entry(appimage: Option<&std::ffi::OsStr>) -> bool {
    appimage.is_none()
}

/// At launch, on Linux: while the system manages the login item, an
/// entry an earlier build wrote goes, now or at the exit
/// (`packaged::remove_earlier_entry`).
#[cfg(target_os = "linux")]
pub fn at_launch(app: &AppHandle) {
    packaged::remove_earlier_entry(&app.package_info().name);
}

/// An update's relaunch is about to exit (`updater`): the next process
/// runs on in the same unit, so an entry that waits for the exit stays
/// for it.
#[cfg(target_os = "linux")]
static RELAUNCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Called before an update's relaunch runs the shutdown.
#[cfg(target_os = "linux")]
pub fn relaunching() {
    RELAUNCHING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// After the shutdown of an exit, on Linux: an earlier build's entry that
/// waited for the exit, because the app ran as the unit made from it,
/// goes now (`packaged::remove_earlier_entry_at_exit`). Not for an
/// update's relaunch; an Xfce query's relaunch never runs as that unit.
#[cfg(target_os = "linux")]
pub fn at_exit(app: &AppHandle) {
    packaged::remove_earlier_entry_at_exit(
        &app.package_info().name,
        RELAUNCHING.load(std::sync::atomic::Ordering::Relaxed),
    );
}

/// Where the user manages login items; `None` where there is no such
/// pane to open (Linux desktops differ).
pub fn system_settings_url() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("x-apple.systempreferences:com.apple.LoginItems-Settings.extension")
    } else if cfg!(target_os = "windows") {
        Some("ms-settings:startupapps")
    } else {
        None
    }
}

/// The host's login item: the plugin's registration, and the pane where
/// the user manages login items. Called with the host's lock held, so it
/// leaves the tray's check mark to its callers (`actions`, and `main.rs`
/// after the launch sequence), which run on the main thread.
pub struct ShellLoginItem {
    pub app: AppHandle,
}

impl steno_host::services::LoginItem for ShellLoginItem {
    fn status(&self) -> LoginItemStatus {
        status(&self.app)
    }

    fn set_enabled(&self, enabled: bool) -> BoundaryResult<()> {
        set_enabled(&self.app, enabled).map_err(|error| error.message.into())
    }

    fn open_system_settings(&self) {
        if let Some(url) = system_settings_url()
            && let Err(error) = crate::dialogs::open_url(&self.app, url)
        {
            tracing::warn!(%error, "opening the login items pane failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_reads_the_plugins_answer() {
        assert_eq!(
            status_from_plugin(Ok::<bool, String>(true)),
            LoginItemStatus::Enabled
        );
        assert_eq!(
            status_from_plugin(Ok::<bool, String>(false)),
            LoginItemStatus::NotRegistered
        );
        assert_eq!(
            status_from_plugin(Err::<bool, _>("no desktop entry")),
            LoginItemStatus::NotFound
        );
        assert!(status_from_plugin(Ok::<bool, String>(true)).is_on());
        assert!(!status_from_plugin(Ok::<bool, String>(false)).is_on());
    }

    /// While the system manages the login item, the plugin is neither
    /// asked nor changed, and the tray's item cannot switch it.
    #[test]
    fn a_managed_login_item_asks_and_changes_nothing() {
        let asked = || -> Result<bool, String> { panic!("the plugin was asked") };
        assert_eq!(status_unless_managed(true, asked), LoginItemStatus::Managed);
        assert_eq!(
            status_unless_managed(false, || Ok::<bool, String>(true)),
            LoginItemStatus::Enabled
        );
        let changed = || -> Result<(), String> { panic!("the plugin changed it") };
        assert!(change_unless_managed(true, changed).is_ok());
        let refused = change_unless_managed(false, || Err("no stable path")).unwrap_err();
        assert_eq!(refused.message, "no stable path");
        assert!(!switchable(LoginItemStatus::Managed));
        assert!(switchable(LoginItemStatus::Enabled));
        assert!(switchable(LoginItemStatus::NotRegistered));
    }

    /// On Linux the shell writes the entry itself, except in an
    /// `AppImage`, where the plugin's names `$APPIMAGE`.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_appimage_keeps_the_plugins_entry() {
        assert!(writes_own_entry(None));
        assert!(!writes_own_entry(Some(std::ffi::OsStr::new(
            "/home/ada/Steno.AppImage"
        ))));
    }

    #[test]
    fn only_macos_and_windows_have_a_pane() {
        let url = system_settings_url();
        if cfg!(target_os = "linux") {
            assert_eq!(url, None);
        } else {
            assert!(url.is_some_and(|url| url.contains(':')));
        }
    }
}
