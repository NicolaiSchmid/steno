//! The Tauri shell (WP3 and WP8 of
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md`): three windows and two
//! floating panels around the web UI in `apps/macos/web`, a tray icon, and
//! the `bridge_call` command the page's Tauri transport talks to. The shell
//! holds no logic: with the default `fixture-host` feature the bridge is
//! answered from the recorded fixtures, so the whole UI runs on Linux and
//! Windows before any pipeline exists; `WP6b` swaps the host for the real one.
//!
//! What the shell owns beside the windows (WP8): the tray (`tray`), the
//! panels (`panels`), launch at login (`autostart`), updates (`updater`),
//! the keyring (`secrets`), the OS permissions (`permissions`), the
//! `steno:` links (`deep_links`), the native dialogs (`dialogs`) and the
//! single instance. Every one is a thin module over a Tauri plugin or an
//! OS API with its rules in plain functions the tests cover. Everything
//! that is on the wire (errors, topics, windows, sections, params) is the
//! `steno-bridge` crate's type; the shell adds only what it needs on top
//! (`recording::RecorderState`, `windows::Spec`).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// Without the fixture host nothing emits a snapshot or publishes a request
// yet; those paths stay compiled so WP6b wires them instead of rewriting them.
#![cfg_attr(not(feature = "fixture-host"), allow(dead_code))]

mod actions;
mod autostart;
mod bridge;
mod deep_links;
mod dialogs;
#[cfg(feature = "fixture-host")]
mod fixtures;
mod host;
#[cfg(target_os = "macos")]
mod menu;
mod navigation;
mod panel_geometry;
mod panels;
mod permissions;
mod recording;
mod secrets;
mod smoke;
mod tray;
mod updater;
mod windows;

use tauri::Manager;

use crate::windows::BridgeWindow;

fn main() {
    let mut builder = tauri::Builder::default();
    // First, so a second instance exits before it builds anything; it
    // hands its arguments (a `steno:` link among them) to this one and
    // the main window comes forward.
    if single_instance_available() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            actions::open(app, windows::BridgeWindow::Main);
        }));
    }
    builder = builder
        .plugin(autostart::plugin())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(updater::plugin());
    #[cfg(target_os = "macos")]
    {
        // The menu bar's menu is the shell's own: Tauri's default one ends
        // in a Quit that terminates without `ExitRequested`.
        builder = builder
            .plugin(tauri_nspanel::init())
            .enable_macos_default_menu(false)
            .menu(menu::build);
    }
    let app = builder
        // One handler for every menu: the tray's on every platform and
        // the menu bar's on macOS reach the same listeners.
        .on_menu_event(|app, event| tray::on_menu_event(app, &event))
        .manage(host::Host)
        .manage(smoke::Smoke::default())
        .manage(panels::Panels::default())
        .manage(windows::Pages::default())
        .manage(updater::Updates::default())
        .invoke_handler(tauri::generate_handler![
            bridge::bridge_call,
            bridge::panel_call
        ])
        .setup(|app| {
            let handle = app.handle();
            // No tray is not fatal: the windows still work, and the process
            // still stays alive for them. On Linux the tray crate panics
            // (rather than errs) when libayatana-appindicator is not
            // installed, so the panic is caught here; the .deb depends on
            // the library and the AppImage bundles it.
            let built =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tray::build(handle)));
            match built {
                Ok(Ok(())) => handle.state::<smoke::Smoke>().note_tray(),
                Ok(Err(error)) => {
                    eprintln!("[steno-desktop] the tray could not be built: {error}");
                }
                Err(_) => eprintln!(
                    "[steno-desktop] the tray could not be built: the tray library is missing"
                ),
            }
            windows::open(handle, windows::BridgeWindow::Main, None, None)?;
            deep_links::install(handle);
            smoke::arm(handle);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("steno-desktop failed to build");
    app.run(|app, event| match event {
        tauri::RunEvent::ExitRequested { code, api, .. } => {
            if !exits_on(code, has_tray(app)) {
                api.prevent_exit();
            }
        }
        // The main window closes: hidden and kept while a tray can bring
        // it back, as the Swift main window closes behind the menu bar
        // item (and the tray's recorder commands keep a window to go
        // through); destroyed otherwise, as the window is then the only
        // way to the app, which ends the process rather than linger
        // invisibly.
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } => {
            if label == BridgeWindow::Main.as_str() && has_tray(app) {
                api.prevent_close();
                if let Some(main) = app.get_webview_window(&label)
                    && let Err(error) = main.hide()
                {
                    eprintln!("[steno-desktop] hiding the main window failed: {error}");
                }
            }
        }
        // A window is gone: its page no longer listens.
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::Destroyed,
            ..
        } => app.state::<windows::Pages>().gone(&label),
        // A panel the user dragged: its anchor follows (`panels::moved`
        // tells a drag from the window taking its size).
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::Moved(position),
            ..
        } => {
            if let Some(panel) = panels::Panel::from_label(&label) {
                panels::moved(app, panel, position);
            }
        }
        // The Dock icon was clicked with no window open: the main window
        // comes back, as it does for the Swift app.
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen {
            has_visible_windows: false,
            ..
        } => actions::open(app, windows::BridgeWindow::Main),
        _ => {}
    });
}

/// Whether the tray was built (`tray::build` manages `Tray` on success).
fn has_tray(app: &tauri::AppHandle) -> bool {
    app.try_state::<tray::Tray>().is_some()
}

/// Whether an exit request ends the process. One with a code is the shell's
/// own (`AppHandle::exit` from Quit, the smoke's) and always does. One
/// without comes from the last window closing: with a tray the process
/// stays, as the Swift menu bar app stays when its window closes
/// (`applicationShouldTerminateAfterLastWindowClosed` in `StenoApp.swift`);
/// without one it ends.
fn exits_on(code: Option<i32>, has_tray: bool) -> bool {
    code.is_some() || !has_tray
}

/// Whether the single-instance plugin can run: on Linux it holds a name on
/// the session bus and panics without one (a headless CI run under
/// `xvfb-run` has none), so it is skipped there; macOS and Windows need
/// nothing.
fn single_instance_available() -> bool {
    !cfg!(target_os = "linux") || std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_shells_own_exit_ends_the_process_while_a_tray_stands() {
        assert!(exits_on(Some(0), true));
        assert!(exits_on(Some(1), true));
        assert!(!exits_on(None, true));
        // No tray: the last window closing ends the process.
        assert!(exits_on(None, false));
        assert!(exits_on(Some(0), false));
    }

    #[test]
    fn single_instance_needs_a_session_bus_on_linux_only() {
        if cfg!(target_os = "linux") {
            assert_eq!(
                single_instance_available(),
                std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
            );
        } else {
            assert!(single_instance_available());
        }
    }
}
