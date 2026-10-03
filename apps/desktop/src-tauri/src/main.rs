//! The Tauri shell (WP3 of `.plans/2026-10-02-rust-core-and-tauri-shell.md`):
//! three windows around the web UI in `apps/macos/web` and the `bridge_call`
//! command the page's Tauri transport talks to. The shell holds no logic:
//! with the default `fixture-host` feature the bridge is answered from the
//! recorded fixtures, so the whole UI runs on Linux and Windows before any
//! pipeline exists; WP6 swaps the host for the real one.
//!
//! Seven shapes duplicate the `steno-bridge` crate's until WP6 wires the
//! crates into the shell, then become `use` lines: `BridgeErrorCode`,
//! `BridgeError`, `BridgeEvent` (whose `topic` becomes the `BridgeTopic`
//! enum), `OpenUrlParams`, `SettingsSection` and `WindowParams` in
//! `bridge.rs`, `BridgeWindow` in `windows.rs`; `bridge::uuid_text` becomes
//! `steno_core::json::uuid_string`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// Without the fixture host nothing emits a snapshot or publishes a request
// yet; those paths stay compiled so WP6 wires them instead of rewriting them.
#![cfg_attr(not(feature = "fixture-host"), allow(dead_code))]

mod bridge;
#[cfg(feature = "fixture-host")]
mod fixtures;
mod host;
mod navigation;
mod smoke;
mod windows;

fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(host::Host)
        .manage(smoke::Smoke::default())
        .invoke_handler(tauri::generate_handler![bridge::bridge_call])
        .setup(|app| {
            let handle = app.handle();
            windows::open(handle, windows::BridgeWindow::Main, None, None)?;
            smoke::arm(handle);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("steno-desktop failed to build");
    // WP6: on macOS the menu's Quit item (muda's predefined `terminate:`) ends
    // the process without `ExitRequested`; tao implements only
    // `applicationWillTerminate`. Once the shell holds state, a graceful
    // shutdown needs a custom Quit item that calls `AppHandle::exit`.
    app.run(|_app, event| {
        let tauri::RunEvent::ExitRequested { code, api, .. } = &event else {
            return;
        };
        if !exits_on(*code) {
            api.prevent_exit();
        }
    });
}

/// Whether an exit request ends the process. One with a code is the shell's
/// own (`AppHandle::exit`, the smoke's) and always does. One without comes
/// from the last window closing: on macOS the process stays, as the Swift
/// menu bar app does (`applicationShouldTerminateAfterLastWindowClosed` in
/// `StenoApp.swift`); Linux and Windows quit until the tray lands in WP8.
fn exits_on(code: Option<i32>) -> bool {
    code.is_some() || !cfg!(target_os = "macos")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_window_keeps_the_process_only_on_macos() {
        assert!(exits_on(Some(0)));
        assert!(exits_on(Some(1)));
        assert_eq!(exits_on(None), !cfg!(target_os = "macos"));
    }
}
