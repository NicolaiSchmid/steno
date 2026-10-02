//! What the tray, the dock and a deep link can do without a page: start or
//! stop the recorder, open a window, quit. Recorder commands are bridge
//! methods the host answers (`recording.toggle`, `recording.start`), sent
//! through the main window as if its page had sent them, so the shell
//! holds no recorder logic of its own.
//!
//! Swift: `MenuBarView.swift` (the buttons), `AppCommands` in
//! `StenoApp.swift` (the Record menu).

use serde_json::{Value, json};
use tauri::{AppHandle, Manager, WebviewWindow};

use crate::{
    bridge::BridgeError,
    host::Host,
    updater,
    windows::{self, BridgeWindow},
};

/// `captureMode` in `contract.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    /// The tray starts a call through `recording.toggle`; a deep link or
    /// the host may ask for the mode by name.
    #[allow(dead_code)]
    Call,
    InPerson,
}

impl CaptureMode {
    /// The raw value on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::InPerson => "inPerson",
        }
    }
}

/// The bridge method and params a tray action sends.
pub fn recorder_command(mode: Option<CaptureMode>) -> (&'static str, Value) {
    match mode {
        None => ("recording.toggle", Value::Null),
        Some(mode) => ("recording.start", json!({ "mode": mode.as_str() })),
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
    Ok(windows::open(app, BridgeWindow::Main, None, None)?)
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
        eprintln!("[steno-desktop] opening the {window} window failed: {error}");
    }
}

/// Checks for an update and offers it, off the caller's thread; the
/// tray's item and `updates.check` from Settings both ask.
pub fn check_for_updates(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move { updater::check_and_offer(&app).await });
}

/// Ends the process through the run loop, so `ExitRequested` carries a code
/// and `main` lets it through (a code-less request is the last window
/// closing, which the tray keeps alive).
pub fn quit(app: &AppHandle) {
    app.exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

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
            [CaptureMode::Call, CaptureMode::InPerson]
                .iter()
                .any(|mode| mode.as_str() == recorded),
            "{recorded}"
        );
    }
}
