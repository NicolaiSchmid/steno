//! The Tauri shell (WP3 and WP8 of
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md`): three windows and two
//! floating panels around the web UI in `apps/macos/web`, a tray icon, and
//! the `bridge_call` command the page's Tauri transport talks to. The shell
//! holds no logic: with the default `fixture-host` feature the bridge is
//! answered from the recorded fixtures, so the whole UI runs on Linux and
//! Windows before any pipeline exists; `WP6b` swaps the host for the real one.
//!
//! What the shell owns beside the windows (WP8): the tray (`tray`), the
//! macOS menu bar (`menu`), the actions behind both menus (`actions`), the
//! recorder state the shell follows (`recording`), the panels (`panels`)
//! and their geometry (`panel_geometry`), window lifetime (`windows`; the
//! close and exit rules are in this file), launch at login (`autostart`),
//! updates (`updater`), the OS permissions (`permissions`), the `steno:`
//! links (`deep_links`), the native dialogs (`dialogs`), the single
//! instance, and on a Wayland session the `XWayland` backend the panels
//! need (`display_backend`, in this file). Secrets are not the shell's:
//! the keyring `SecretStore` lives in `steno-services` (#173, `WP6b`).
//! Every one is a thin module over a Tauri plugin or an OS API with its
//! rules in plain functions the tests cover. Everything that is on the
//! wire (errors, topics, windows, sections, params) is the `steno-bridge`
//! crate's type; the shell adds only what it needs on top
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
mod smoke;
mod tray;
mod updater;
mod windows;

#[cfg(target_os = "linux")]
use std::ffi::OsStr;

use tauri::Manager;

use crate::windows::BridgeWindow;

fn main() {
    #[cfg(target_os = "linux")]
    use_xwayland_on_wayland();
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
        .on_menu_event(|app, event| actions::on_menu_event(app, &event))
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
            // No tray is not fatal: the windows still work, and closing
            // main then ends the process (`exits_when_destroyed`). On Linux
            // the tray crate panics (rather than errs) when
            // libayatana-appindicator is not installed, so the panic is
            // caught here; the .deb depends on the library, the AppImage
            // does not bundle it (README, Bundles).
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
        // it back (`hides_on_close`); destroyed otherwise.
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } => {
            if hides_on_close(&label, has_tray(app)) {
                api.prevent_close();
                if let Some(main) = app.get_webview_window(&label)
                    && let Err(error) = main.hide()
                {
                    eprintln!("[steno-desktop] hiding the main window failed: {error}");
                }
            }
        }
        // A window is gone: its page no longer listens; a destroyed main
        // window with no tray ends the process (`exits_when_destroyed`).
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::Destroyed,
            ..
        } => {
            app.state::<windows::Pages>().gone(&label);
            if exits_when_destroyed(&label, has_tray(app)) {
                actions::quit(app);
            }
        }
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

/// Whether closing the window of `label` hides it instead: the main window
/// while a tray can bring it back, as the Swift main window closes behind
/// the menu bar item (and the tray's recorder commands keep a window to go
/// through). Every other close destroys the window.
fn hides_on_close(label: &str, has_tray: bool) -> bool {
    label == BridgeWindow::Main.as_str() && has_tray
}

/// Whether the window of `label` being destroyed ends the process: the
/// main window with no tray, as it was then the only way to the app. The
/// panels' windows are hidden, never destroyed, so once one has existed
/// the last window never closes on its own, and the process would linger
/// invisibly.
fn exits_when_destroyed(label: &str, has_tray: bool) -> bool {
    label == BridgeWindow::Main.as_str() && !has_tray
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

/// Which GDK backend the shell runs on (`display_backend`).
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayBackend {
    /// No Wayland session: GTK picks, which is X11.
    GtksChoice,
    /// A Wayland session with `XWayland`: the shell sets
    /// `GDK_BACKEND=x11` and runs under it.
    ForcedX11,
    /// A Wayland session without `XWayland` (no `DISPLAY`): X11 would not
    /// open, so GTK runs on Wayland and the panels neither float nor stay
    /// where they are put.
    WaylandOnly,
    /// The user's `GDK_BACKEND`, whatever it says.
    Users,
}

#[cfg(target_os = "linux")]
impl DisplayBackend {
    /// The startup log line; it names no value from the environment.
    const fn describe(self) -> &'static str {
        match self {
            Self::GtksChoice => "display: GTK's default backend (no Wayland session)",
            Self::ForcedX11 => {
                "display: a Wayland session, so the shell runs under XWayland \
                 (GDK_BACKEND=x11) to keep the panels on top and where they are put; \
                 a GDK_BACKEND set before launch overrides this"
            }
            Self::WaylandOnly => {
                "display: a Wayland session without XWayland, so the panels \
                 neither stay on top nor keep their place"
            }
            Self::Users => "display: the GDK_BACKEND set before launch",
        }
    }
}

/// The GDK backend for a session with these `WAYLAND_DISPLAY`, `DISPLAY`
/// and `GDK_BACKEND` values (an empty value counts as none, except the
/// user's `GDK_BACKEND`). On Wayland GTK 3 can neither place a top-level
/// window nor keep it above the others, and reports no moves, so the
/// panels would neither float nor stay where they are put nor save their
/// anchor; under `XWayland` all three work. A `GDK_BACKEND` the user set,
/// even to `wayland` or empty, always wins.
#[cfg(target_os = "linux")]
fn display_backend(
    wayland_display: Option<&OsStr>,
    x11_display: Option<&OsStr>,
    gdk_backend: Option<&OsStr>,
) -> DisplayBackend {
    let set = |value: Option<&OsStr>| value.is_some_and(|value| !value.is_empty());
    if gdk_backend.is_some() {
        DisplayBackend::Users
    } else if !set(wayland_display) {
        DisplayBackend::GtksChoice
    } else if set(x11_display) {
        DisplayBackend::ForcedX11
    } else {
        DisplayBackend::WaylandOnly
    }
}

/// Applies `display_backend` to this process and logs it once. Runs first
/// in `main`.
#[cfg(target_os = "linux")]
fn use_xwayland_on_wayland() {
    let backend = display_backend(
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        std::env::var_os("DISPLAY").as_deref(),
        std::env::var_os("GDK_BACKEND").as_deref(),
    );
    if backend == DisplayBackend::ForcedX11 {
        // SAFETY: this is the first statement of `main`, before the Tauri
        // builder, the tokio runtime, GTK or any plugin exists, so the
        // process has one thread and nothing reads the environment
        // concurrently. Nothing before `main` starts one: the only
        // constructor in the tree (`tauri-utils`' starting binary) reads
        // the executable's path and spawns nothing.
        unsafe { std::env::set_var("GDK_BACKEND", "x11") };
    }
    eprintln!("[steno-desktop] {}", backend.describe());
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

    /// With a tray, closing main hides it and the process stays; without
    /// one, main is destroyed and the process ends with it, whatever else
    /// (a hidden panel) is still around. The other windows just close.
    #[test]
    fn main_hides_behind_a_tray_and_ends_the_process_without_one() {
        assert!(hides_on_close("main", true));
        assert!(!hides_on_close("main", false));
        assert!(!exits_when_destroyed("main", true));
        assert!(exits_when_destroyed("main", false));
        for label in ["settings", "onboarding", "bubble", "prompt"] {
            for tray in [true, false] {
                assert!(!hides_on_close(label, tray), "{label}");
                assert!(!exits_when_destroyed(label, tray), "{label}");
            }
        }
    }

    /// A Wayland session runs under `XWayland` when there is one, unless
    /// the user chose a backend; an X11 session, or an empty
    /// `WAYLAND_DISPLAY`, is left to GTK.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_wayland_session_runs_under_xwayland_unless_the_user_chose() {
        let wayland = Some(OsStr::new("wayland-0"));
        let x11 = Some(OsStr::new(":0"));
        assert_eq!(
            display_backend(wayland, None, None),
            DisplayBackend::WaylandOnly
        );
        assert_eq!(display_backend(None, x11, None), DisplayBackend::GtksChoice);
        assert_eq!(
            display_backend(wayland, x11, None),
            DisplayBackend::ForcedX11
        );
        assert_eq!(
            display_backend(wayland, x11, Some(OsStr::new("wayland"))),
            DisplayBackend::Users
        );
        assert_eq!(
            display_backend(None, x11, Some(OsStr::new("x11"))),
            DisplayBackend::Users
        );
        assert_eq!(
            display_backend(wayland, x11, Some(OsStr::new(""))),
            DisplayBackend::Users
        );
        assert_eq!(
            display_backend(Some(OsStr::new("")), x11, None),
            DisplayBackend::GtksChoice
        );
        assert_eq!(
            display_backend(wayland, Some(OsStr::new("")), None),
            DisplayBackend::WaylandOnly
        );
        for backend in [
            DisplayBackend::GtksChoice,
            DisplayBackend::ForcedX11,
            DisplayBackend::WaylandOnly,
            DisplayBackend::Users,
        ] {
            assert!(backend.describe().starts_with("display: "));
        }
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
