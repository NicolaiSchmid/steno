//! Launch at login over `tauri-plugin-autostart`: a Launch Agent on macOS,
//! the `autostart` desktop entry on Linux, the Run registry key on Windows.
//! The Swift app registers itself with `SMAppService`, whose
//! `requiresApproval` state has no Launch Agent counterpart and never
//! occurs here. The status is the host's `LoginItemStatus`, and
//! `ShellLoginItem` is the host's `LoginItem` over this module (`WP6b`), so
//! the General section reads and switches the real registration.
//!
//! Swift: `LoginItemController.swift`, `LoginItemStatus` in `AppProtocols.swift`.

use steno_core::protocols::BoundaryResult;
pub use steno_host::services::LoginItemStatus;
use tauri::AppHandle;
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use crate::bridge::{BridgeError, failed};

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

pub fn status(app: &AppHandle) -> LoginItemStatus {
    status_from_plugin(app.autolaunch().is_enabled())
}

/// Registers or removes the login item; a plugin failure is `failed`,
/// which the page shows as it would any other refused command.
pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), BridgeError> {
    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    result.map_err(failed)
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
