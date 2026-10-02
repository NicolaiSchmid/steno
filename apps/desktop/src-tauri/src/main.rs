//! The Tauri shell (WP3 of `.plans/2026-10-02-rust-core-and-tauri-shell.md`):
//! three windows around the web UI in `apps/macos/web` and the `bridge_call`
//! command the page's Tauri transport talks to. The shell holds no logic:
//! with the default `fixture-host` feature the bridge is answered from the
//! recorded fixtures, so the whole UI runs on Linux and Windows before any
//! pipeline exists; WP6 swaps the host for the real one.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
#[cfg(feature = "fixture-host")]
mod fixtures;
mod host;
mod navigation;
mod smoke;
mod windows;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(host::Host)
        .invoke_handler(tauri::generate_handler![bridge::bridge_call])
        .setup(|app| {
            let handle = app.handle();
            windows::open(handle, windows::Kind::Main, None, None)?;
            smoke::arm(handle);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("steno-desktop failed to run");
}
