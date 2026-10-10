//! The tray icon: the Swift menu bar item's menu as a native menu on every
//! platform. On macOS it is the menu bar extra (a template icon, so it
//! follows the bar's appearance); on Linux it is a status notifier item and
//! on Windows a notification area icon, both with the app icon. The menu
//! is the Swift popover's controls without its queue and recent rows, which
//! the main window shows: Record (or Stop), Record in person, Open Steno,
//! Settings, Launch at login, Check for Updates, Quit (Exit on Windows).
//! Only the Mac's menu shows shortcut hints (`shortcut`).
//!
//! Every action is in the menu (`MENU`), and a click on the icon does
//! nothing the menu does not: macOS and Windows open the menu on a left click as on a
//! right one, and on Linux the host decides what a left click does. KDE
//! Plasma and GNOME's `AppIndicator` extension open the menu; a host that
//! sends `Activate` instead, as Omarchy's bar may, gets no answer from
//! libayatana-appindicator, and there only a right click opens the menu.
//!
//! The tray also keeps the process alive: with it, closing the main window
//! hides it and the process stays, as the Swift menu bar app stays; without
//! a tray the window closes and the process quits with it, saving a
//! recording in progress first (`main.rs`). On Linux a tray that was built
//! still counts as none while nothing shows its icon (`has_host`).
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

use steno_bridge::Platform;

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

/// A menu item's shortcut hint, on the Mac only: the menu bar extra's
/// menu shows one as the Swift menus did, while Windows' notification
/// area menu and a Linux status notifier menu show none, and a key
/// pressed there with the menu closed does nothing.
const fn shortcut(platform: Platform, action: MenuAction) -> Option<&'static str> {
    match (platform, action) {
        (Platform::Macos, MenuAction::Record) => Some("Cmd+Shift+R"),
        (Platform::Macos, MenuAction::OpenSettings) => Some("Cmd+,"),
        (Platform::Macos, MenuAction::Quit) => Some("Cmd+Q"),
        _ => None,
    }
}

/// One row of the tray's menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Item(MenuAction),
    Separator,
}

/// The tray's menu, top to bottom: every `MenuAction` once, so nothing
/// waits on a click the host may not pass on. Quit is the shell's own
/// item, not muda's predefined Quit, which ends the process without
/// `ExitRequested`, so the shell could not shut down cleanly.
const MENU: [Row; 10] = [
    Row::Item(MenuAction::Record),
    Row::Item(MenuAction::RecordInPerson),
    Row::Separator,
    Row::Item(MenuAction::OpenMain),
    Row::Item(MenuAction::OpenSettings),
    Row::Separator,
    Row::Item(MenuAction::LaunchAtLogin),
    Row::Item(MenuAction::CheckForUpdates),
    Row::Separator,
    Row::Item(MenuAction::Quit),
];

/// Builds the menu and the icon and manages `Tray`.
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let platform = Platform::CURRENT;
    let record = MenuAction::Record.item(app, shortcut(platform, MenuAction::Record))?;
    let in_person = MenuAction::RecordInPerson.item(app, None)?;
    // A login item the system manages shows checked and cannot be
    // switched (`autostart`).
    let login_item = autostart::status(app);
    let launch_at_login = CheckMenuItem::with_id(
        app,
        MenuAction::LaunchAtLogin.as_str(),
        MenuAction::LaunchAtLogin.label(),
        autostart::switchable(login_item),
        login_item.is_on(),
        None::<&str>,
    )?;
    let menu = Menu::new(app)?;
    for row in MENU {
        match row {
            Row::Separator => menu.append(&PredefinedMenuItem::separator(app)?)?,
            Row::Item(MenuAction::Record) => menu.append(&record)?,
            Row::Item(MenuAction::RecordInPerson) => menu.append(&in_person)?,
            Row::Item(MenuAction::LaunchAtLogin) => menu.append(&launch_at_login)?,
            Row::Item(action) => menu.append(&action.item(app, shortcut(platform, action))?)?,
        }
    }
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

/// Whether something on the desktop shows the tray's icon. macOS and
/// Windows always do. On Linux a status notifier host does, as the
/// follower in `tray_host` last read it; until it has, and with no session
/// bus, none does, the safe side: closing main then ends the app instead
/// of leaving it running unseen. Reading it asks nothing of the bus.
pub fn has_host() -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::tray_host::HOSTED.shown()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_mac_menu_shows_shortcuts() {
        assert_eq!(shortcut(Platform::Macos, MenuAction::Quit), Some("Cmd+Q"));
        assert_eq!(
            shortcut(Platform::Macos, MenuAction::Record),
            Some("Cmd+Shift+R")
        );
        assert_eq!(
            shortcut(Platform::Macos, MenuAction::OpenSettings),
            Some("Cmd+,")
        );
        assert_eq!(shortcut(Platform::Macos, MenuAction::OpenMain), None);
        for &action in MenuAction::ALL {
            assert_eq!(shortcut(Platform::Windows, action), None);
            assert_eq!(shortcut(Platform::Linux, action), None);
        }
    }

    /// Every action is in the tray's menu once, so a host that passes no
    /// left click on still reaches each; separators only between items.
    #[test]
    fn the_menu_holds_every_action_once() {
        for &action in MenuAction::ALL {
            let rows = MENU.iter().filter(|&&row| row == Row::Item(action));
            assert_eq!(rows.count(), 1, "{action}");
        }
        assert_ne!(MENU.first(), Some(&Row::Separator));
        assert_ne!(MENU.last(), Some(&Row::Separator));
        assert!(
            MENU.windows(2)
                .all(|pair| pair != [Row::Separator, Row::Separator])
        );
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
