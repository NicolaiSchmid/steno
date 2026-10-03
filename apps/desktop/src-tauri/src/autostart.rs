//! Launch at login over `tauri-plugin-autostart`: a Launch Agent on macOS,
//! the `autostart` desktop entry on Linux, the Run registry key on Windows.
//! The Swift app registers itself with `SMAppService`, whose
//! `requiresApproval` state has no Launch Agent counterpart; the enum keeps
//! the Swift cases so the General section's snapshot can carry them
//! unchanged once the host reads this module.
//!
//! Swift: `LoginItemController.swift`, `LoginItemStatus` in `AppProtocols.swift`.

use tauri::AppHandle;
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use crate::bridge::{BridgeError, failed};

/// `GeneralSettingsSnapshot.launchAtLogin` in the contract. The host reads
/// it for the General section (`WP6b`); the shell reads only `is_on`.
/// `steno_host::services::LoginItemStatus` is the same set; `WP6b` keeps
/// that one when it implements the host's `LoginItem` over this module.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum LoginItemStatus {
    NotRegistered,
    Enabled,
    /// Never produced here; `SMAppService` only.
    RequiresApproval,
    /// The plugin could not read the registration; the message says why.
    NotFound(String),
}

impl LoginItemStatus {
    /// The raw value on the wire.
    #[allow(dead_code)]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::NotRegistered => "notRegistered",
            Self::Enabled => "enabled",
            Self::RequiresApproval => "requiresApproval",
            Self::NotFound(_) => "notFound",
        }
    }

    /// What the switch shows.
    pub const fn is_on(&self) -> bool {
        matches!(self, Self::Enabled)
    }

    /// From the plugin's answer.
    pub fn from_plugin(result: Result<bool, impl std::fmt::Display>) -> Self {
        match result {
            Ok(true) => Self::Enabled,
            Ok(false) => Self::NotRegistered,
            Err(error) => Self::NotFound(error.to_string()),
        }
    }
}

/// The plugin, configured as the Swift app behaves: a Launch Agent (no
/// `AppleScript` prompt), no launch arguments.
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None)
}

pub fn status(app: &AppHandle) -> LoginItemStatus {
    LoginItemStatus::from_plugin(app.autolaunch().is_enabled())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_reads_the_plugins_answer() {
        assert_eq!(
            LoginItemStatus::from_plugin(Ok::<bool, String>(true)),
            LoginItemStatus::Enabled
        );
        assert_eq!(
            LoginItemStatus::from_plugin(Ok::<bool, String>(false)),
            LoginItemStatus::NotRegistered
        );
        assert_eq!(
            LoginItemStatus::from_plugin(Err::<bool, _>("no desktop entry")),
            LoginItemStatus::NotFound("no desktop entry".into())
        );
    }

    #[test]
    fn the_wire_values_are_the_swift_ones() {
        assert_eq!(LoginItemStatus::NotRegistered.as_str(), "notRegistered");
        assert_eq!(LoginItemStatus::Enabled.as_str(), "enabled");
        assert_eq!(
            LoginItemStatus::RequiresApproval.as_str(),
            "requiresApproval"
        );
        assert_eq!(
            LoginItemStatus::NotFound(String::new()).as_str(),
            "notFound"
        );
        assert!(LoginItemStatus::Enabled.is_on());
        assert!(!LoginItemStatus::RequiresApproval.is_on());
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
