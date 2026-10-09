//! What the tray, the menu bar, the dock and a deep link can do without a
//! page: start or stop the recorder, open a window, switch launch at
//! login, check for updates, quit. The menus' items are `MenuAction`s,
//! and `on_menu_event` is the one handler for both menus (the tray's,
//! `tray.rs`, and on macOS the menu bar's, `menu.rs`). Recorder commands
//! are bridge methods the host answers (`recording.toggle`,
//! `recording.start`), sent through the main window as if its page had
//! sent them, so the shell holds no recorder logic of its own.
//!
//! Swift: `MenuBarView.swift` (the buttons), `AppCommands` in
//! `StenoApp.swift` (the Record menu).

use serde_json::{Value, json};
pub use steno_bridge::CaptureMode;
use steno_bridge::Platform;
use tauri::{
    AppHandle, Manager, WebviewWindow, Wry,
    menu::{MenuEvent, MenuId, MenuItem},
};

use crate::{
    autostart,
    bridge::{BridgeError, failed},
    host::Host,
    tray, updater,
    windows::{self, BridgeWindow},
};

steno_core::string_enum! {
    /// The menu items, by id (`as_str`). The ids are stable strings so a
    /// test can map them both ways without a menu.
    pub enum MenuAction {
        Record = "record",
        RecordInPerson = "record-in-person",
        OpenMain = "open-main",
        OpenSettings = "open-settings",
        LaunchAtLogin = "launch-at-login",
        CheckForUpdates = "check-for-updates",
        Quit = "quit",
    }
}

impl MenuAction {
    /// The item's text at rest on this platform ([`Self::label_on`]);
    /// `record_label` replaces the first while the recorder is busy.
    pub const fn label(self) -> &'static str {
        self.label_on(Platform::CURRENT)
    }

    /// The item's text on `platform`: the Mac's "Settings…", "Check for
    /// Updates…" and "Quit Steno", Windows' "Exit Steno", and on Windows
    /// and Linux "Settings" and "Check for Updates" without the ellipsis,
    /// which there marks an item that asks for more input before it acts;
    /// both of these act at once (the update dialog is the check's result).
    pub const fn label_on(self, platform: Platform) -> &'static str {
        match self {
            Self::Record => "Record",
            Self::RecordInPerson => "Record in person",
            Self::OpenMain => "Open Steno",
            Self::OpenSettings => platform.mac_or("Settings…", "Settings"),
            Self::LaunchAtLogin => "Launch at login",
            Self::CheckForUpdates => platform.mac_or("Check for Updates…", "Check for Updates"),
            Self::Quit => match platform {
                Platform::Windows => "Exit Steno",
                Platform::Macos | Platform::Linux => "Quit Steno",
            },
        }
    }

    /// The plain menu item for the action, in the tray's menu or the menu
    /// bar's.
    pub fn item(self, app: &AppHandle, accelerator: Option<&str>) -> tauri::Result<MenuItem<Wry>> {
        MenuItem::with_id(app, self.as_str(), self.label(), true, accelerator)
    }
}

/// The bridge method and params a tray action sends. The tray starts a
/// call through `recording.toggle` and names the mode only for an
/// in-person recording.
pub fn recorder_command(mode: Option<CaptureMode>) -> (&'static str, Value) {
    match mode {
        None => ("recording.toggle", Value::Null),
        Some(mode) => ("recording.start", json!({ "mode": mode })),
    }
}

/// The window the host answers tray commands through: the main window,
/// created when the user closed it. The host publishes the resulting
/// `recording` snapshot there, which is also where the tray and the
/// panels read it.
pub fn host_window(app: &AppHandle) -> Result<WebviewWindow, BridgeError> {
    if let Some(window) = app.get_webview_window(BridgeWindow::Main.as_str()) {
        return Ok(window);
    }
    windows::open(app, BridgeWindow::Main, None, None).map_err(failed)
}

/// Starts (`Some(mode)`) or toggles (`None`) the recorder through the host.
pub fn record(app: &AppHandle, mode: Option<CaptureMode>) -> Result<(), BridgeError> {
    let window = host_window(app)?;
    let (method, params) = recorder_command(mode);
    app.state::<Host>().call(&window, method, params)?;
    Ok(())
}

/// Opens or focuses a window; errors are logged, as a menu item has no one
/// to report them to.
pub fn open(app: &AppHandle, window: BridgeWindow) {
    if let Err(error) = windows::open(app, window, None, None) {
        stderr_line!("[steno-desktop] opening the {window} window failed: {error}");
    }
}

/// Checks for an update and offers it, off the caller's thread; the
/// tray's item and `updates.check` from Settings both ask.
pub fn check_for_updates(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move { updater::check_and_offer(&app).await });
}

/// A menu item was chosen, in the tray's menu or the menu bar's.
pub fn on_menu_event(app: &AppHandle, event: &MenuEvent) {
    let MenuId(id) = event.id();
    let Ok(action) = id.parse::<MenuAction>() else {
        return;
    };
    match action {
        MenuAction::Record => report(record(app, None)),
        MenuAction::RecordInPerson => report(record(app, Some(CaptureMode::InPerson))),
        MenuAction::OpenMain => open(app, BridgeWindow::Main),
        MenuAction::OpenSettings => open(app, BridgeWindow::Settings),
        MenuAction::LaunchAtLogin => {
            let wanted = !autostart::status(app).is_on();
            report(host_window(app).and_then(|window| set_launch_at_login(app, &window, wanted)));
            // The check mark follows the system, even when the change failed.
            tray::note_login_item(app);
        }
        MenuAction::CheckForUpdates => check_for_updates(app),
        MenuAction::Quit => quit(app),
    }
}

fn report(result: Result<(), BridgeError>) {
    if let Err(error) = result {
        stderr_line!("[steno-desktop] menu: {error}");
    }
}

/// The bridge method and params that tell the host the login item
/// changed: the same call Settings > General makes, so its snapshot
/// follows whichever menu switched it.
pub fn launch_at_login_command(value: bool) -> (&'static str, Value) {
    (
        "settings.general.setLaunchAtLogin",
        json!({ "value": value }),
    )
}

/// Switches launch at login, moves the tray's check mark and tells the
/// host through `window`, for the tray's item and for Settings alike.
/// The host hears of a failed switch too: it tries again and, failing,
/// shows why in Settings > General.
/// Swift: `MenuBarViewModel.setLaunchAtLogin`.
pub fn set_launch_at_login(
    app: &AppHandle,
    window: &WebviewWindow,
    value: bool,
) -> Result<(), BridgeError> {
    let switched = autostart::set_enabled(app, value);
    let (method, params) = launch_at_login_command(value);
    let told = app.state::<Host>().call(window, method, params);
    tray::note_login_item(app);
    switched?;
    told?;
    Ok(())
}

/// The code Quit exits with.
pub const QUIT_CODE: i32 = 0;

/// Ends the process through the run loop, so `ExitRequested` carries a code
/// and `main` lets it through (a code-less request is the last window
/// closing, which the tray keeps alive) once the shutdown ran: a recording
/// in progress is saved first (`main::exit_request`). Quit in the tray's
/// menu and in the macOS menu bar, a destroyed main window with no tray,
/// and SIGTERM, SIGINT and SIGHUP all end here.
pub fn quit(app: &AppHandle) {
    app.exit(QUIT_CODE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_round_trips_through_its_id() {
        for &action in MenuAction::ALL {
            assert_eq!(action.as_str().parse::<MenuAction>(), Ok(action));
            assert_eq!(action.to_string(), action.as_str());
            assert_ne!(action.label(), "");
        }
        assert!("about".parse::<MenuAction>().is_err());
    }

    #[test]
    fn each_platform_names_settings_updates_and_quit_its_own_way() {
        let labels = |platform| {
            (
                MenuAction::OpenSettings.label_on(platform),
                MenuAction::CheckForUpdates.label_on(platform),
                MenuAction::Quit.label_on(platform),
            )
        };
        assert_eq!(
            labels(Platform::Macos),
            ("Settings…", "Check for Updates…", "Quit Steno")
        );
        assert_eq!(
            labels(Platform::Windows),
            ("Settings", "Check for Updates", "Exit Steno")
        );
        assert_eq!(
            labels(Platform::Linux),
            ("Settings", "Check for Updates", "Quit Steno")
        );
        assert_eq!(
            MenuAction::Quit.label(),
            MenuAction::Quit.label_on(Platform::CURRENT)
        );
    }

    /// The tray's login item tells the host what Settings tells it, in the
    /// recorded params shape.
    #[test]
    fn the_login_item_tells_the_host_as_settings_does() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../macos/web/fixtures/bridge/params.bool.json"
        ))
        .unwrap();
        let value = fixture["value"].as_bool().unwrap();
        assert_eq!(
            launch_at_login_command(value),
            ("settings.general.setLaunchAtLogin", fixture)
        );
    }

    #[test]
    fn the_tray_sends_the_bridge_methods_the_page_sends() {
        assert_eq!(recorder_command(None), ("recording.toggle", Value::Null));
        assert_eq!(
            recorder_command(Some(CaptureMode::Call)),
            ("recording.start", json!({ "mode": "call" }))
        );
        assert_eq!(
            recorder_command(Some(CaptureMode::InPerson)),
            ("recording.start", json!({ "mode": "inPerson" }))
        );
    }

    #[test]
    fn the_mode_spells_as_the_contract_does() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../macos/web/fixtures/bridge/params.recording.start.json"
        ))
        .unwrap();
        let recorded = fixture["mode"].as_str().unwrap();
        assert!(
            CaptureMode::ALL
                .iter()
                .any(|mode| mode.as_str() == recorded),
            "{recorded}"
        );
    }
}
