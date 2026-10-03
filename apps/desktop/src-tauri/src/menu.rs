//! The menu bar's menu on macOS. Tauri's default menu ends in a predefined
//! Quit that calls `terminate:` outright, with no `ExitRequested` for the
//! shell to see, so the shell builds its own: the application menu (About,
//! Settings…, Check for Updates…, Services, Hide, Hide Others, Show All,
//! and Quit Steno as the tray's item, which quits through
//! `actions::quit` and the run loop), the Edit menu whose predefined items
//! give the webviews their undo, cut, copy, paste and select-all
//! shortcuts, and the Window menu. The items share the tray's ids, so the
//! one handler `main.rs` registers (`tray::on_menu_event`) serves both
//! menus. Linux and Windows show no menu bar; the tray carries the
//! actions there.
//!
//! Swift: `AppCommands` in `StenoApp.swift`.

use tauri::{
    AppHandle, Wry,
    menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu},
};

use crate::tray::MenuAction;

/// The whole menu bar; `Builder::menu` installs it.
pub fn build(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let item = |action: MenuAction, accelerator: Option<&str>| {
        MenuItem::with_id(app, action.id(), action.label(), true, accelerator)
    };
    let info = app.package_info();
    let about = AboutMetadata {
        name: Some(info.name.clone()),
        version: Some(info.version.to_string()),
        ..AboutMetadata::default()
    };
    let application = Submenu::with_items(
        app,
        "Steno",
        true,
        &[
            &PredefinedMenuItem::about(app, None, Some(about))?,
            &PredefinedMenuItem::separator(app)?,
            &item(MenuAction::OpenSettings, Some("Cmd+,"))?,
            &item(MenuAction::CheckForUpdates, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &item(MenuAction::Quit, Some("Cmd+Q"))?,
        ],
    )?;
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;
    Menu::with_items(app, &[&application, &edit, &window])
}
