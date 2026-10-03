//! The tray icon: the Swift menu bar item's menu as a native menu on every
//! platform. On macOS it is the menu bar extra (a template icon, so it
//! follows the bar's appearance); on Linux it is a status notifier item and
//! on Windows a notification area icon, both with the app icon. The menu
//! is the Swift popover's controls without its queue and recent rows, which
//! the main window shows: Record (or Stop), Record in person, Open Steno,
//! Settings, Launch at login, Check for Updates, Quit.
//!
//! The tray also keeps the process alive: with it, closing the main window
//! hides it and the process stays, as the Swift menu bar app stays; without
//! a tray (a Linux desktop with no indicator host) the window closes and
//! the process ends with it (`main.rs`).
//!
//! The items are `actions::MenuAction`s and their handler is
//! `actions::on_menu_event`, registered once by `main.rs`: Tauri hands
//! every menu event to the same listeners, whether from the tray's menu
//! or, on macOS, the menu bar's (`menu.rs`), whose items carry the same
//! ids.
//!
//! Swift: `MenuBarView.swift`, `MenuBarLabel` and `MenuBarLabelPresentation`
//! in `StenoApp.swift` and `FloatingContent.swift`.

use tauri::{
    AppHandle, Manager, Wry,
    image::Image,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
};

use crate::{
    actions::MenuAction,
    autostart,
    recording::{RecorderState, RecordingState},
};

/// The tray's id, for `AppHandle::tray_by_id`.
pub const TRAY_ID: &str = "steno";

/// The Record item's text per recorder state: the same words as the
/// sidebar's control (`RecordButton` in `sidebar.tsx`).
pub const fn record_label(state: RecordingState) -> &'static str {
    match state {
        RecordingState::Idle => "Record",
        RecordingState::Starting => "Starting…",
        RecordingState::Recording => "Stop recording",
        RecordingState::Stopping => "Stopping…",
    }
}

/// Whether the Record item does anything: not while the recorder is
/// between states.
pub const fn record_enabled(state: RecordingState) -> bool {
    matches!(state, RecordingState::Idle | RecordingState::Recording)
}

/// "Record in person" is offered only to an idle recorder
/// (`RecordingControlPresentation.offersInPerson`).
pub const fn in_person_enabled(state: RecordingState) -> bool {
    matches!(state, RecordingState::Idle)
}

/// The icon's tooltip, the Swift label's accessibility text: "Steno" or
/// "Steno, recording" (the elapsed time is the bubble's, not the tray's).
pub fn tooltip(state: RecordingState) -> &'static str {
    if state.is_busy() {
        "Steno, recording"
    } else {
        "Steno"
    }
}

/// The live items, kept as managed state so a `recording` snapshot can
/// retitle them.
pub struct Tray {
    record: MenuItem<Wry>,
    in_person: MenuItem<Wry>,
    launch_at_login: CheckMenuItem<Wry>,
}

/// Builds the menu and the icon and manages `Tray`.
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let record = MenuAction::Record.item(app, Some("CmdOrCtrl+Shift+R"))?;
    let in_person = MenuAction::RecordInPerson.item(app, None)?;
    let launch_at_login = CheckMenuItem::with_id(
        app,
        MenuAction::LaunchAtLogin.id(),
        MenuAction::LaunchAtLogin.label(),
        true,
        autostart::status(app).is_on(),
        None::<&str>,
    )?;
    let menu = Menu::with_items(
        app,
        &[
            &record,
            &in_person,
            &PredefinedMenuItem::separator(app)?,
            &MenuAction::OpenMain.item(app, None)?,
            &MenuAction::OpenSettings.item(app, Some("CmdOrCtrl+,"))?,
            &PredefinedMenuItem::separator(app)?,
            &launch_at_login,
            &MenuAction::CheckForUpdates.item(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            // Not muda's predefined Quit: that one ends the process without
            // `ExitRequested`, so the shell could not shut down cleanly.
            &MenuAction::Quit.item(app, Some("CmdOrCtrl+Q"))?,
        ],
    )?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip(tooltip(RecordingState::Idle));
    builder = if cfg!(target_os = "macos") {
        // The four-bar mark as a template image: the menu bar draws it in
        // its own colour, as it drew the SF Symbol.
        builder
            .icon(Image::from_bytes(include_bytes!(
                "../icons/tray/mark@2x.png"
            ))?)
            .icon_as_template(true)
    } else if let Some(icon) = app.default_window_icon() {
        builder.icon(icon.clone())
    } else {
        builder
    };
    builder.build(app)?;
    app.manage(Tray {
        record,
        in_person,
        launch_at_login,
    });
    Ok(())
}

/// A `recording` snapshot reached the main window: retitle the items.
pub fn note_recording(app: &AppHandle, state: RecordingState) {
    let Some(tray) = app.try_state::<Tray>() else {
        return;
    };
    let _ = tray.record.set_text(record_label(state));
    let _ = tray.record.set_enabled(record_enabled(state));
    let _ = tray.in_person.set_enabled(in_person_enabled(state));
    if let Some(icon) = app.tray_by_id(TRAY_ID) {
        let _ = icon.set_tooltip(Some(tooltip(state)));
    }
}

/// The login item changed (from the menu or from Settings): the check mark
/// follows what the system says, not what was asked.
pub fn note_login_item(app: &AppHandle) {
    if let Some(tray) = app.try_state::<Tray>() {
        let _ = tray
            .launch_at_login
            .set_checked(autostart::status(app).is_on());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_item_follows_the_recorder() {
        assert_eq!(record_label(RecordingState::Idle), "Record");
        assert_eq!(record_label(RecordingState::Starting), "Starting…");
        assert_eq!(record_label(RecordingState::Recording), "Stop recording");
        assert_eq!(record_label(RecordingState::Stopping), "Stopping…");
        assert!(record_enabled(RecordingState::Idle));
        assert!(record_enabled(RecordingState::Recording));
        assert!(!record_enabled(RecordingState::Starting));
        assert!(!record_enabled(RecordingState::Stopping));
    }

    #[test]
    fn in_person_is_offered_to_an_idle_recorder_only() {
        assert!(in_person_enabled(RecordingState::Idle));
        for state in [
            RecordingState::Starting,
            RecordingState::Recording,
            RecordingState::Stopping,
        ] {
            assert!(!in_person_enabled(state), "{state:?}");
        }
    }

    #[test]
    fn the_tooltip_is_the_swift_accessibility_label() {
        assert_eq!(tooltip(RecordingState::Idle), "Steno");
        assert_eq!(tooltip(RecordingState::Starting), "Steno, recording");
        assert_eq!(tooltip(RecordingState::Recording), "Steno, recording");
    }

    #[test]
    fn the_template_icon_is_a_png() {
        let bytes = include_bytes!("../icons/tray/mark@2x.png");
        assert_eq!(&bytes[1..4], b"PNG");
        assert!(Image::from_bytes(bytes).is_ok());
    }
}
