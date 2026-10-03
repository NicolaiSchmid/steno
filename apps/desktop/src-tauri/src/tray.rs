//! The tray icon: the Swift menu bar item's menu as a native menu on every
//! platform. On macOS it is the menu bar extra (a template icon, so it
//! follows the bar's appearance); on Linux it is a status notifier item and
//! on Windows a notification area icon, both with the app icon. The menu
//! is the Swift popover's controls without its queue and recent rows, which
//! the main window shows: Record (or Stop), Record in person, Open Steno,
//! Settings, Launch at login, Check for Updates, Quit.
//!
//! The tray also keeps the process alive: with it, closing the last window
//! ends nothing on any platform (`main.rs`).
//!
//! Swift: `MenuBarView.swift`, `MenuBarLabel` and `MenuBarLabelPresentation`
//! in `StenoApp.swift` and `FloatingContent.swift`.

use std::fmt;

use tauri::{
    AppHandle, Manager, Wry,
    image::Image,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
};

use crate::{
    actions::{self, CaptureMode},
    autostart,
    recording::{RecorderState, RecordingState},
    windows::BridgeWindow,
};

/// The tray's id, for `AppHandle::tray_by_id`.
pub const TRAY_ID: &str = "steno";

/// The menu items, by id. The ids are stable strings so a test can map
/// them both ways without a menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Record,
    RecordInPerson,
    OpenMain,
    OpenSettings,
    LaunchAtLogin,
    CheckForUpdates,
    Quit,
}

impl MenuAction {
    pub const ALL: [MenuAction; 7] = [
        Self::Record,
        Self::RecordInPerson,
        Self::OpenMain,
        Self::OpenSettings,
        Self::LaunchAtLogin,
        Self::CheckForUpdates,
        Self::Quit,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Record => "record",
            Self::RecordInPerson => "record-in-person",
            Self::OpenMain => "open-main",
            Self::OpenSettings => "open-settings",
            Self::LaunchAtLogin => "launch-at-login",
            Self::CheckForUpdates => "check-for-updates",
            Self::Quit => "quit",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.id() == id)
    }

    /// The item's text at rest; `record_label` replaces the first while
    /// the recorder is busy.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Record => "Record",
            Self::RecordInPerson => "Record in person",
            Self::OpenMain => "Open Steno",
            Self::OpenSettings => "Settings…",
            Self::LaunchAtLogin => "Launch at login",
            Self::CheckForUpdates => "Check for Updates…",
            Self::Quit => "Quit Steno",
        }
    }
}

impl fmt::Display for MenuAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

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
    let plain = |action: MenuAction, accelerator: Option<&str>| {
        MenuItem::with_id(app, action.id(), action.label(), true, accelerator)
    };
    let record = plain(MenuAction::Record, Some("CmdOrCtrl+Shift+R"))?;
    let in_person = plain(MenuAction::RecordInPerson, None)?;
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
            &plain(MenuAction::OpenMain, None)?,
            &plain(MenuAction::OpenSettings, Some("CmdOrCtrl+,"))?,
            &PredefinedMenuItem::separator(app)?,
            &launch_at_login,
            &plain(MenuAction::CheckForUpdates, None)?,
            &PredefinedMenuItem::separator(app)?,
            // Not muda's predefined Quit: that one ends the process without
            // `ExitRequested`, so the shell could not shut down cleanly.
            &plain(MenuAction::Quit, Some("CmdOrCtrl+Q"))?,
        ],
    )?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip(tooltip(RecordingState::Idle))
        .on_menu_event(|app, event| on_menu_event(app, &event));
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

fn on_menu_event(app: &AppHandle, event: &MenuEvent) {
    let MenuId(id) = event.id();
    let Some(action) = MenuAction::from_id(id) else {
        return;
    };
    match action {
        MenuAction::Record => report(actions::record(app, None)),
        MenuAction::RecordInPerson => report(actions::record(app, Some(CaptureMode::InPerson))),
        MenuAction::OpenMain => actions::open(app, BridgeWindow::Main),
        MenuAction::OpenSettings => actions::open(app, BridgeWindow::Settings),
        MenuAction::LaunchAtLogin => {
            let wanted = !autostart::status(app).is_on();
            if let Err(error) = autostart::set_enabled(app, wanted) {
                eprintln!("[steno-desktop] login item could not be changed: {error}");
            }
            note_login_item(app);
        }
        MenuAction::CheckForUpdates => actions::check_for_updates(app),
        MenuAction::Quit => actions::quit(app),
    }
}

fn report(result: Result<(), crate::bridge::BridgeError>) {
    if let Err(error) = result {
        eprintln!("[steno-desktop] tray: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_round_trips_through_its_id() {
        for action in MenuAction::ALL {
            assert_eq!(MenuAction::from_id(action.id()), Some(action));
            assert_eq!(action.to_string(), action.id());
            assert_ne!(action.label(), "");
        }
        assert_eq!(MenuAction::from_id("about"), None);
    }

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
