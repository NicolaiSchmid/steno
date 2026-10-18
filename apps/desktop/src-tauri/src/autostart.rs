//! Launch at login: `SMAppService.mainApp` on macOS, `tauri-plugin-autostart`
//! (the autostart desktop entry, the Run registry key) on Linux and Windows.
//! `ShellLoginItem` is the host's `LoginItem` over this module.

mod main_app;

use steno_core::protocols::BoundaryResult;
pub use steno_host::services::LoginItemStatus;
use tauri::AppHandle;
#[cfg(not(target_os = "macos"))]
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use crate::bridge::{BridgeError, failed};
use crate::packaged;

/// The status from the plugin's answer; an unreadable one is `NotFound`.
#[cfg(not(target_os = "macos"))]
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

/// The plugin, with no launch arguments (Linux and Windows only).
#[cfg(not(target_os = "macos"))]
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None)
}

/// The login item as the user set it: `Managed` while the system starts
/// the app at login.
pub fn status(app: &AppHandle) -> LoginItemStatus {
    status_unless_managed(packaged::login_item_is_managed(), || {
        #[cfg(target_os = "macos")]
        {
            let _ = app;
            main_app::MainApp::status(&main_app::SystemMainApp)
        }
        #[cfg(not(target_os = "macos"))]
        {
            status_from_plugin(app.autolaunch().is_enabled())
        }
    })
}

fn status_unless_managed(managed: bool, read: impl FnOnce() -> LoginItemStatus) -> LoginItemStatus {
    if managed {
        return LoginItemStatus::Managed;
    }
    read()
}

/// Whether the tray's item may switch the login item (not a managed one).
pub fn switchable(status: LoginItemStatus) -> bool {
    status != LoginItemStatus::Managed
}

/// Registers or removes the login item; a plugin failure is `failed`.
/// Changes nothing while the system manages the login item.
pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), BridgeError> {
    change_unless_managed(packaged::login_item_is_managed(), || {
        #[cfg(target_os = "macos")]
        {
            let _ = app;
            main_app::set_enabled(&main_app::SystemMainApp, enabled).map_err(failed)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let result = if enabled {
                enable(app)
            } else {
                app.autolaunch().disable()
            };
            result.map_err(failed)
        }
    })
}

fn change_unless_managed(
    managed: bool,
    change: impl FnOnce() -> Result<(), BridgeError>,
) -> Result<(), BridgeError> {
    if managed {
        tracing::debug!("Launch at login is the system's; the login item stays as it is");
        return Ok(());
    }
    change()
}

/// Writes the login item: on Linux outside an `AppImage` the shell's own
/// entry with a stable path, elsewhere the plugin's.
#[cfg(not(target_os = "macos"))]
fn enable(app: &AppHandle) -> Result<(), tauri_plugin_autostart::Error> {
    #[cfg(target_os = "linux")]
    if writes_own_entry(tauri::Manager::env(app).appimage.as_deref()) {
        return Ok(packaged::write_entry(&app.package_info().name)?);
    }
    app.autolaunch().enable()
}

/// Whether the shell writes the Linux entry itself: outside an `AppImage`,
/// whose plugin entry already names a stable path.
#[cfg(target_os = "linux")]
fn writes_own_entry(appimage: Option<&std::ffi::OsStr>) -> bool {
    appimage.is_none()
}

/// At launch, on Linux, while the system manages the login item: an entry
/// an earlier build wrote goes.
#[cfg(target_os = "linux")]
pub fn remove_earlier_entry(app: &AppHandle) {
    packaged::remove_earlier_entry(&app.package_info().name);
}

/// At launch, on macOS: removes the Launch Agent a build under the earlier
/// identifier left behind. Nothing goes in a smoke run, a `fixture-host`
/// build or a run from outside an installed bundle, which would remove the
/// agent an installed Steno still starts.
#[cfg(target_os = "macos")]
pub fn remove_earlier_agent() {
    let exe = std::env::current_exe().ok();
    if !main_app::removes_earlier_agent(main_app::smoke_run(), exe.as_deref()) {
        return;
    }
    main_app::log(&main_app::remove_earlier_agent(
        &main_app::UserLaunchAgents::of_home(),
        std::process::id(),
    ));
}

/// Whether this app runs from an installed app bundle. On the Mac, a run
/// from elsewhere registers no login item at its first launch; a bundle
/// built under `target/` counts as installed. Off the Mac it is unused.
#[cfg(not(feature = "fixture-host"))]
pub fn installed_bundle() -> bool {
    std::env::current_exe().is_ok_and(|exe| main_app::installed_bundle(&exe))
}

/// Where the user manages login items; `None` on Linux.
pub fn system_settings_url() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("x-apple.systempreferences:com.apple.LoginItems-Settings.extension")
    } else if cfg!(target_os = "windows") {
        Some("ms-settings:startupapps")
    } else {
        None
    }
}

/// The host's login item. Called with the host's lock held, so the tray's
/// check mark is left to its callers on the main thread.
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

    /// macOS registers with `SMAppService` only; the plugin is off macOS.
    #[test]
    fn the_autostart_plugin_is_neither_built_nor_registered_on_macos() {
        let manifest = include_str!("../Cargo.toml");
        let mut table = "";
        let mut tables = Vec::new();
        for line in manifest.lines() {
            if line.starts_with('[') {
                table = line;
            } else if line.starts_with("tauri-plugin-autostart") {
                tables.push(table);
            }
        }
        assert_eq!(
            tables,
            ["[target.'cfg(not(target_os = \"macos\"))'.dependencies]"]
        );

        let main = include_str!("main.rs").replace("\r\n", "\n");
        assert_eq!(main.matches("autostart::plugin()").count(), 1);
        assert!(main.contains(
            "    #[cfg(not(target_os = \"macos\"))]\n    {\n        builder = builder.plugin(autostart::plugin());\n    }\n"
        ));
    }

    #[cfg(not(target_os = "macos"))]
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

    /// While managed, the plugin is neither asked nor changed.
    #[test]
    fn a_managed_login_item_asks_and_changes_nothing() {
        let asked = || -> LoginItemStatus { panic!("the system was asked") };
        assert_eq!(status_unless_managed(true, asked), LoginItemStatus::Managed);
        assert_eq!(
            status_unless_managed(false, || LoginItemStatus::RequiresApproval),
            LoginItemStatus::RequiresApproval
        );
        let changed = || -> Result<(), BridgeError> { panic!("the plugin changed it") };
        assert!(change_unless_managed(true, changed).is_ok());
        let refused = change_unless_managed(false, || Err(failed("no stable path"))).unwrap_err();
        assert_eq!(refused.message, "no stable path");
        assert!(!switchable(LoginItemStatus::Managed));
        assert!(switchable(LoginItemStatus::Enabled));
        assert!(switchable(LoginItemStatus::NotRegistered));
    }

    /// The shell writes the Linux entry itself, except in an `AppImage`.
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
