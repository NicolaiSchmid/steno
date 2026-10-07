//! The Tauri shell (WP3, WP8 and WP6b of
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md`): three windows and two
//! floating panels around the web UI in `apps/macos/web`, a tray icon, and
//! the `bridge_call` command the page's Tauri transport talks to. The shell
//! holds no logic: the bridge is answered by `steno_host::Host` over the
//! services graph (`host`); with the opt-in `fixture-host` feature it is
//! answered from the recorded fixtures instead, so the UI runs without a
//! database.
//!
//! What the shell owns beside the windows (WP8): the tray (`tray`), the
//! macOS menu bar (`menu`), the actions behind both menus (`actions`), the
//! recorder state the shell follows (`recording`), the panels (`panels`)
//! and their geometry (`panel_geometry`), window lifetime (`windows`; the
//! close and exit rules are in this file), launch at login (`autostart`),
//! updates (`updater`), the OS permissions (`permissions`), the `steno:`
//! links (`deep_links`), the native dialogs (`dialogs`), the single
//! instance, on Linux the logout and shutdown clients (`session_end`), and
//! on a Wayland session the `XWayland` backend the panels need
//! (`display`). Every one is a thin module over a Tauri plugin or an
//! OS API with its rules in plain functions the tests cover. Everything
//! that is on the wire (errors, topics, windows, sections, params) is the
//! `steno-bridge` crate's type; the shell adds only what it needs on top
//! (`recording::RecorderState`, `windows::Spec`). Secrets are not the
//! shell's: the keyring `SecretStore` lives in `steno-services` (#173,
//! `WP6b`).
//!
//! Every exit runs `App::shutdown` first, at most `SHUTDOWN_PATIENCE` (ten
//! seconds), over one `ExitGate`: it quits the pipeline (no new job
//! starts), stops and saves a recording in progress and stops the handover
//! listener. Quit from either menu, the close that ends the process when no
//! tray stands, and on Linux and macOS SIGTERM, SIGINT and SIGHUP (a plain
//! `kill`, Ctrl-C, a closed terminal, systemd at a shutdown) are exit
//! requests the gate holds (`exit_request`, `exit_on_signals`); a signal
//! quits the pipeline at once, before its request reaches the main thread.
//! A signal the app inherited ignored stays ignored (`ignored`). The Dock's
//! Quit, a logout and a shutdown on macOS, and a logoff and a shutdown on
//! Windows, reach the run loop only as its last event, and an update's
//! relaunch bypasses the request, so both run the same shutdown first
//! (`shut_down_before_exit`). The one exception: a second SIGTERM or a
//! second SIGINT ends the process at once, unsaved (`forced_exit`). On
//! Linux a logout on GNOME or Xfce, through the session manager, a logout
//! where the desktop portal reports the session's end, and a system
//! shutdown or reboot run the same shutdown before they let the app go,
//! over D-Bus (`session_end`); the display closing under the app at the end
//! of any session runs it before GDK ends the process (`display_lost`);
//! and an exit that went through ends the process `EXIT_GRACE` later at the
//! latest (`end_within`). Open: the Windows logoff is untested on hardware,
//! and Windows' end-session timeout (about five seconds) is shorter than
//! `SHUTDOWN_PATIENCE` (WP10); the Linux paths have not run on a real
//! GNOME or KDE Plasma session (before the first Linux release).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// The fixture host leaves the real host's seams (the login item, the
// opener, the alert) unused.
#![cfg_attr(feature = "fixture-host", allow(dead_code))]

/// `eprintln!` for the shell's own lines, minus its panic: once the
/// terminal the app started from has closed, every write to stderr fails,
/// and the line is dropped instead.
macro_rules! stderr_line {
    ($($line:tt)*) => {
        $crate::write_stderr_line(format_args!($($line)*))
    };
}

/// What `stderr_line!` writes.
fn write_stderr_line(line: std::fmt::Arguments<'_>) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "{line}");
}

mod actions;
mod autostart;
mod bridge;
mod deep_links;
mod dialogs;
#[cfg(target_os = "linux")]
mod display;
#[cfg(target_os = "linux")]
mod display_lost;
#[cfg(feature = "fixture-host")]
mod fixtures;
mod host;
#[cfg(target_os = "macos")]
mod menu;
mod navigation;
mod panel_geometry;
mod panels;
mod permissions;
mod platform;
mod recording;
#[cfg(target_os = "linux")]
mod session_end;
mod smoke;
mod tray;
mod updater;
mod windows;

use tauri::Manager;

use crate::windows::BridgeWindow;

fn main() {
    steno_services::log_to_stderr(steno_services::LOG_FILTER);
    // After the log output, whose hook it runs after its own: a panic
    // leaves a file under the support directory, since an app opened from
    // the Finder or at login has no stderr anyone reads.
    steno_core::crash_log::install_crash_log_hook(
        steno_core::StenoPaths::default_support_directory(),
        None,
    );
    #[cfg(target_os = "linux")]
    {
        display::choose();
        display_lost::watch();
    }
    // Before the app is built: GTK unsets it when it starts.
    #[cfg(target_os = "linux")]
    let startup_id = session_end::startup_id();
    // The runtime the services graph runs on, beside Tauri's own: the
    // pipeline, the recorder's saves, the handover listener and the signal
    // listeners. Leaked, so it is never dropped: dropping a runtime waits,
    // without a bound, for every blocking task on it (a transcription, a
    // model load), and Tauri drops whatever owns it before the process
    // exits (the `setup` closure; the run loop's handler on Linux and
    // Windows).
    let runtime: &'static tokio::runtime::Runtime = Box::leak(Box::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime"),
    ));
    let mut builder = tauri::Builder::default();
    // First, so a second instance exits before it builds anything; it
    // hands its arguments (a `steno:` link among them) to this one and
    // the main window comes forward.
    if single_instance_available() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if host::is_running(app) {
                actions::open(app, windows::BridgeWindow::Main);
            }
        }));
    }
    builder = builder
        .plugin(autostart::plugin())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(updater::plugin())
        .plugin(platform::plugin());
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
        .on_menu_event(|app, event| {
            if host::is_running(app) {
                actions::on_menu_event(app, &event);
            }
        })
        .manage(smoke::Smoke::default())
        .manage(panels::Panels::default())
        .manage(windows::Pages::default())
        .manage(windows::Retired::default())
        .manage(updater::Updates::default())
        .manage(TrayAtClose::default())
        .manage(steno_services::app::ExitGate::default())
        .invoke_handler(tauri::generate_handler![
            bridge::bridge_call,
            bridge::panel_call
        ])
        .setup(move |app| {
            if setup(app.handle(), runtime)? {
                #[cfg(target_os = "linux")]
                {
                    session_end::watch(app.handle(), startup_id);
                    display_lost::arm(app.handle());
                }
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("steno-desktop failed to build");
    app.run(on_event);
}

/// Builds the host, the tray and the main window, opens onboarding when
/// the host asks for it, and runs the launch sequence. False when the
/// Swift app runs, another process holds the database, or the host cannot
/// be built (the database cannot be opened or read): this app then shows
/// why and ends (`refuse_to_start`), with no host, tray or window.
fn setup(
    handle: &tauri::AppHandle,
    runtime: &'static tokio::runtime::Runtime,
) -> Result<bool, Box<dyn std::error::Error>> {
    #[cfg(not(feature = "fixture-host"))]
    if let Some(refusal) = refusal_before_build(platform_app_running) {
        refuse_to_start(
            handle,
            refusal,
            &format_args!("{SWIFT_BUNDLE_ID} is running"),
        );
        return Ok(false);
    }
    #[cfg(not(feature = "fixture-host"))]
    let host = match host::Host::real(handle, runtime) {
        Ok(host) => host,
        Err(error) => {
            refuse_to_start(handle, Refusal::after(&error), &error);
            return Ok(false);
        }
    };
    #[cfg(feature = "fixture-host")]
    let host = host::Host::fixtures();
    let onboarding = host.should_open_onboarding();
    handle.manage(host);
    build_tray(handle);
    windows::open(handle, windows::BridgeWindow::Main, None, None)?;
    deep_links::install(handle);
    // Before onboarding opens, so a smoke run places it.
    smoke::arm(handle);
    if onboarding {
        windows::open(handle, windows::BridgeWindow::Onboarding, None, None)?;
    }
    host::host(handle).launch(runtime);
    // The launch may have registered the login item.
    tray::note_login_item(handle);
    #[cfg(unix)]
    exit_on_signals(handle, runtime);
    Ok(true)
}

/// The exit code of an app that refused to start (`refuse_to_start`).
const REFUSED_CODE: i32 = 3;

/// How long a refused app waits for its dialog to be closed before it ends
/// anyway (`refuse_to_start`).
const REFUSED_PATIENCE: std::time::Duration = std::time::Duration::from_secs(60);

/// The Swift app's bundle id. It takes no database lock
/// (`steno_core::DatabaseLock`), so this app looks for it by name.
const SWIFT_BUNDLE_ID: &str = "uno.schmid.steno.mac";

/// Why this app refuses to start (`refuse_to_start`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The Swift app runs: its launch and this one's would fail each
    /// other's live recording, and it takes no lock to say so.
    OlderSteno,
    /// Another process holds the database: another app (an app under the
    /// old identifier beside one under the new, one the single-instance
    /// guard missed: Linux without a session bus, a failed connect on the
    /// Mac), or a `steno` command.
    DatabaseHeld,
    /// The host could not be built for another reason: the database cannot
    /// be opened, migrated or read, or the support directory cannot be
    /// created. The error goes to the log. Nothing in the database is lost:
    /// what may have committed before the failure (a migration, the
    /// admission backfill) only adds.
    Unavailable,
}

impl Refusal {
    /// The refusal for a host that could not be built:
    /// [`Refusal::DatabaseHeld`] while another process holds the database,
    /// else [`Refusal::Unavailable`].
    #[cfg(not(feature = "fixture-host"))]
    fn after(error: &host::ShellHostError) -> Self {
        if error.is_database_held() {
            Refusal::DatabaseHeld
        } else {
            Refusal::Unavailable
        }
    }

    fn title(self) -> &'static str {
        match self {
            Refusal::OlderSteno => "An older Steno is running",
            Refusal::DatabaseHeld => "Steno is already running",
            Refusal::Unavailable => "Steno could not start",
        }
    }

    fn text(self) -> &'static str {
        match self {
            Refusal::OlderSteno => "Quit the older Steno app first, then open Steno again.",
            Refusal::DatabaseHeld => {
                "Another Steno is already open, or a steno command is running in a terminal. \
                 Quit it, then open Steno again."
            }
            Refusal::Unavailable => {
                "Steno could not open your meetings. They are safe. Open Steno again."
            }
        }
    }
}

/// The refusal due before the host is built, with `running` answering
/// whether an app with a bundle id runs ([`platform_app_running`] in the
/// product): [`Refusal::OlderSteno`] while the Swift app runs. A Swift app
/// started after this one is not kept out.
#[cfg(not(feature = "fixture-host"))]
fn refusal_before_build(running: impl FnOnce(&str) -> bool) -> Option<Refusal> {
    running(SWIFT_BUNDLE_ID).then_some(Refusal::OlderSteno)
}

/// Whether an app with `bundle_id` runs on this Mac; always false
/// elsewhere.
#[cfg(not(feature = "fixture-host"))]
fn platform_app_running(bundle_id: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSRunningApplication;
        use objc2_foundation::NSString;

        NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(
            bundle_id,
        ))
        .count()
            != 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = bundle_id;
        false
    }
}

/// Says why this app does not start ([`Refusal`]), logs `reason`, and ends
/// with [`REFUSED_CODE`] once the dialog is closed, before it opens a window:
/// two apps on one database would fail each other's recordings at launch,
/// and a host that could not be built writes nothing more. Its run loop has
/// no host to shut down (`host::is_running`). Should the dialog never show,
/// or its callback never run, the process ends after [`REFUSED_PATIENCE`]
/// all the same, or after the wait of a smoke run (`STENO_SMOKE_SECONDS`,
/// so the smoke sees the refusal end): it holds nothing to save. Rust only:
/// the Swift app relied on macOS opening one copy per bundle id.
fn refuse_to_start(handle: &tauri::AppHandle, refusal: Refusal, reason: &dyn std::fmt::Display) {
    use tauri_plugin_dialog::{DialogExt as _, MessageDialogButtons, MessageDialogKind};

    stderr_line!("[steno-desktop] not starting: {reason}");
    let patience = match smoke::wait_from(std::env::var(smoke::SECONDS_VARIABLE).ok().as_deref()) {
        smoke::Wait::Seconds(seconds) => std::time::Duration::from_secs(seconds),
        smoke::Wait::NotASmokeRun | smoke::Wait::Invalid(_) => REFUSED_PATIENCE,
    };
    std::thread::spawn(move || {
        std::thread::sleep(patience);
        std::process::exit(REFUSED_CODE);
    });
    let app = handle.clone();
    handle
        .dialog()
        .message(refusal.text())
        .title(refusal.title())
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::Ok)
        .show(move |_| app.exit(REFUSED_CODE));
}

/// A signal that asks for the exit Quit asks for (`exit_on_signals`).
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExitSignal {
    kind: tokio::signal::unix::SignalKind,
    name: &'static str,
    /// The code a second one ends the process with at once, 128 plus the
    /// signal's number, as the shells report it; none for SIGHUP, which
    /// never forces the exit.
    forced_code: Option<i32>,
}

#[cfg(unix)]
impl ExitSignal {
    const TERMINATE: Self = Self {
        kind: tokio::signal::unix::SignalKind::terminate(),
        name: "SIGTERM",
        forced_code: Some(128 + 15),
    };
    const INTERRUPT: Self = Self {
        kind: tokio::signal::unix::SignalKind::interrupt(),
        name: "SIGINT",
        forced_code: Some(128 + 2),
    };
    const HANGUP: Self = Self {
        kind: tokio::signal::unix::SignalKind::hangup(),
        name: "SIGHUP",
        forced_code: None,
    };
    const ALL: [Self; 3] = [Self::TERMINATE, Self::INTERRUPT, Self::HANGUP];
}

/// Whether the signal `number` is ignored now, as `nohup` and a shell's
/// background job (SIGINT) leave it at launch; false when its disposition
/// cannot be read. The app does not listen for such a signal, so it stays
/// ignored: installing a handler would undo it.
#[cfg(unix)]
fn ignored(number: libc::c_int) -> bool {
    // SAFETY: an all-zero `sigaction` is a valid value, and a null new
    // action makes the call read the disposition without changing it.
    let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
    let read = unsafe { libc::sigaction(number, std::ptr::null(), &raw mut current) };
    read == 0 && current.sa_sigaction == libc::SIG_IGN
}

/// What `signal` does after the exit signals already `seen`, which it
/// joins: the code to end the process with at once (a second SIGTERM, a
/// second SIGINT), or none to ask for the Quit exit.
#[cfg(unix)]
fn forced_exit(signal: ExitSignal, seen: &mut Vec<ExitSignal>) -> Option<i32> {
    if seen.contains(&signal) {
        return signal.forced_code;
    }
    seen.push(signal);
    None
}

/// One arrival of `signal`: the code to end the process with at once
/// (`forced_exit`), or none once it has quit the pipeline and then asked
/// for Quit, in that order, so the pipeline is quit before the request
/// waits for the main thread. The guard on `seen` is released before
/// either step.
#[cfg(unix)]
fn on_exit_signal(
    signal: ExitSignal,
    seen: &std::sync::Mutex<Vec<ExitSignal>>,
    quit_pipeline: impl FnOnce(),
    ask_for_quit: impl FnOnce(),
) -> Option<i32> {
    let forced = forced_exit(
        signal,
        &mut seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    if forced.is_none() {
        quit_pipeline();
        ask_for_quit();
    }
    forced
}

/// SIGTERM, SIGINT and SIGHUP ask for the exit Quit asks for, so the
/// shutdown runs first: a plain `kill`, Ctrl-C in a terminal, a closed
/// terminal, systemd at a shutdown. A logout on Linux saves here when
/// logind ends the session's processes (with `KillUserProcesses=yes`,
/// systemd stops the scope with SIGTERM, then SIGHUP); on GNOME and Xfce
/// the session manager's end saves first (`session_end`), and on every
/// desktop the display closing does (`display_lost`). On macOS a logout
/// goes through `RunEvent::Exit` instead.
/// Each signal quits the pipeline here, off the main thread, before it asks
/// for the exit (`Host::quit_pipeline`), so a job a busy main thread would
/// let fail first stays resumable. A second SIGTERM or a second SIGINT
/// ends the process at once, unsaved (`forced_exit`), so a run loop that no
/// longer answers still ends with a plain `kill` or Ctrl-C twice; a SIGHUP
/// never does, so the one that follows a session scope's SIGTERM still
/// saves. A signal the app inherited ignored stays ignored (`ignored`).
/// Swift had no handler; each of them ended the app unsaved.
#[cfg(unix)]
fn exit_on_signals(app: &tauri::AppHandle, runtime: &tokio::runtime::Runtime) {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    for signal in ExitSignal::ALL {
        if ignored(signal.kind.as_raw_value()) {
            tracing::debug!(
                signal = signal.name,
                "ignored at launch, so it stays ignored"
            );
            continue;
        }
        let (app, seen) = (app.clone(), seen.clone());
        runtime.spawn(async move {
            let mut arrivals = match tokio::signal::unix::signal(signal.kind) {
                Ok(arrivals) => arrivals,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        signal = signal.name,
                        "this signal ends the app without saving a recording"
                    );
                    return;
                }
            };
            while arrivals.recv().await.is_some() {
                let forced = on_exit_signal(
                    signal,
                    &seen,
                    || host::host(&app).quit_pipeline(),
                    || actions::quit(&app),
                );
                if let Some(code) = forced {
                    steno_services::flush_logs();
                    std::process::exit(code);
                }
            }
        });
    }
}

/// Runs the shutdown for an exit no request held, on this thread's behalf,
/// and returns once it ended (at most `SHUTDOWN_PATIENCE`), waiting for
/// the one already running (a held request's, an earlier exit's) instead
/// of starting one; at once when it already ran. `RunEvent::Exit`, the
/// updater's relaunch and the Linux logout and shutdown clients
/// (`session_end`) call it, each right before the process ends, so it
/// writes out the queued log lines too.
///
/// Swift: the Dock's Quit and a logout reached `applicationShouldTerminate`
/// as Quit did.
fn shut_down_before_exit(app: &tauri::AppHandle) {
    app.state::<steno_services::app::ExitGate>().exiting(
        steno_services::app::SHUTDOWN_PATIENCE,
        host::host(app).shutdown_action(),
    );
    steno_services::flush_logs();
}

/// The save before an end that no exit request announced, on Linux: the
/// pipeline quits first, as for an exit signal (`Host::quit_pipeline`),
/// then the shutdown runs on the calling thread's behalf
/// (`shut_down_before_exit`). The logout and shutdown clients
/// (`session_end`) and the lost display (`display_lost`) call it.
#[cfg(target_os = "linux")]
fn save_before_end(app: &tauri::AppHandle) {
    host::host(app).quit_pipeline();
    shut_down_before_exit(app);
}

/// One turn of the run loop; nothing for an app that refused to start
/// (`refuse_to_start`), which has no host and ends without a shutdown.
fn on_event(app: &tauri::AppHandle, event: tauri::RunEvent) {
    if !host::is_running(app) {
        return;
    }
    match event {
        // Quit from either menu, an exit signal, the smoke's exit, the
        // last window closing with no tray: the shutdown runs first
        // (`exit_request`).
        tauri::RunEvent::ExitRequested { code, api, .. } => {
            let handle = app.clone();
            if exit_request(
                code,
                || tray_at_close(app),
                &app.state::<steno_services::app::ExitGate>(),
                host::host(app).shutdown_action(),
                move |code| handle.exit(code),
            ) {
                #[cfg(target_os = "linux")]
                if code != Some(tauri::RESTART_EXIT_CODE) {
                    let code = code.unwrap_or(0);
                    end_within(EXIT_GRACE, move || {
                        steno_services::flush_logs();
                        std::process::exit(code);
                    });
                }
            } else {
                api.prevent_exit();
            }
        }
        // The run loop's last event. After a request the gate held, the
        // shutdown has run; the Dock's Quit, a logout and a shutdown on
        // macOS raise no request (tao answers `applicationWillTerminate`,
        // which AppKit waits for), nor do a logoff and a shutdown on
        // Windows (tao answers `WM_ENDSESSION`), so the shutdown runs
        // here, on Windows until the end-session timeout (about five
        // seconds) ends the process.
        tauri::RunEvent::Exit => shut_down_before_exit(app),
        // The main window closes: hidden and kept while a tray can bring
        // it back (`hides_on_close`); destroyed otherwise. `has_tray` is
        // asked here, once per close, and the events that follow reuse it.
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } if label == BridgeWindow::Main.as_str() => {
            let tray = has_tray(app);
            app.state::<TrayAtClose>().note(tray);
            if hides_on_close(&label, tray) {
                api.prevent_close();
                if let Some(main) = app.get_webview_window(&label)
                    && let Err(error) = main.hide()
                {
                    stderr_line!("[steno-desktop] hiding the main window failed: {error}");
                }
            }
        }
        // Settings or onboarding closes on Linux: kept, not destroyed
        // (`windows::retires_on_close`, #160); the host hears of
        // onboarding's close as it does of a destroyed window.
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } if windows::retires_on_close(&label) => {
            api.prevent_close();
            let Some(window) = app.get_webview_window(&label) else {
                return;
            };
            match windows::retire(app, &window) {
                Ok(true) if label == BridgeWindow::Onboarding.as_str() => {
                    host::host(app).onboarding_window_closed();
                }
                Ok(_) => {}
                Err(error) => {
                    stderr_line!("[steno-desktop] keeping the {label} window failed: {error}");
                }
            }
        }
        // A window is gone: its page no longer listens; a destroyed main
        // window with no tray ends the process (`exits_when_destroyed`). A
        // kept window (`windows::Retired`) is destroyed only as the app
        // ends, and the host heard of its close when it was kept.
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::Destroyed,
            ..
        } => {
            app.state::<windows::Pages>().gone(&label);
            let kept = app.state::<windows::Retired>().forget(&label);
            if label == BridgeWindow::Onboarding.as_str() && !kept {
                host::host(app).onboarding_window_closed();
            }
            if exits_when_destroyed(&label, || tray_at_close(app)) {
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
    }
}

/// Builds the tray. No tray is not fatal: the windows still work, and
/// closing main then ends the process (`exits_when_destroyed`). On Linux
/// the tray crate panics (rather than errs) when libayatana-appindicator
/// is not installed, so the panic is caught here; the .deb depends on the
/// library, the `AppImage` does not bundle it (README, Bundles). A tray
/// nothing shows (`tray::has_host`) is logged once here.
fn build_tray(app: &tauri::AppHandle) {
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tray::build(app)));
    match built {
        Ok(Ok(())) => {
            app.state::<smoke::Smoke>().note_tray();
            if !tray::has_host() {
                stderr_line!(
                    "[steno-desktop] no tray host shows the tray icon; \
                     closing the main window ends the app"
                );
            }
        }
        Ok(Err(error)) => stderr_line!("[steno-desktop] the tray could not be built: {error}"),
        Err(_) => {
            stderr_line!(
                "[steno-desktop] the tray could not be built: the tray library is missing"
            );
        }
    }
}

/// Whether a tray stands (`tray_stands`): `tray::build` manages `Tray` on
/// success, and the host is asked last.
fn has_tray(app: &tauri::AppHandle) -> bool {
    tray_stands(
        app.try_state::<tray::Tray>().is_some(),
        app.state::<smoke::Smoke>().is_armed(),
        tray::has_host,
    )
}

/// Whether a tray stands: it was `built` and something shows it, a host
/// (`tray::has_host`, asked only when it decides) or a smoke run, which
/// stands in for the host Xvfb lacks so it checks the close rule a desktop
/// with a tray gets.
fn tray_stands(built: bool, smoke: bool, host: impl FnOnce() -> bool) -> bool {
    built && (smoke || host())
}

/// What `has_tray` said when the main window last closed, so the
/// `Destroyed` and the exit request that follow a close do not ask the
/// session bus again.
#[derive(Default)]
struct TrayAtClose(std::sync::Mutex<Option<bool>>);

impl TrayAtClose {
    fn note(&self, tray: bool) {
        if let Ok(mut noted) = self.0.lock() {
            *noted = Some(tray);
        }
    }

    fn noted(&self) -> Option<bool> {
        self.0.lock().ok().and_then(|noted| *noted)
    }
}

/// The tray as the last close of main found it, else as it is now.
fn tray_at_close(app: &tauri::AppHandle) -> bool {
    app.state::<TrayAtClose>()
        .noted()
        .unwrap_or_else(|| has_tray(app))
}

/// Whether closing the window of `label` hides it instead: the main window
/// while a tray can bring it back, as the Swift main window closes behind
/// the menu bar item (and the tray's recorder commands keep a window to go
/// through). Settings and onboarding are kept on Linux
/// (`windows::retires_on_close`); every other close destroys the window.
fn hides_on_close(label: &str, has_tray: bool) -> bool {
    label == BridgeWindow::Main.as_str() && has_tray
}

/// Whether the window of `label` being destroyed ends the process: the
/// main window with no tray, since nothing else reaches the app; the
/// Swift app always has its menu bar item. The panels' windows are
/// hidden, never destroyed, so once one has existed the last window never
/// closes on its own, and the process would linger invisibly.
fn exits_when_destroyed(label: &str, has_tray: impl FnOnce() -> bool) -> bool {
    label == BridgeWindow::Main.as_str() && !has_tray()
}

/// What an exit request does; true lets it through now. `exits_on`
/// decides whether the process ends at all. The first one that ends it is
/// held: `shutdown` runs on a thread of its own (`App::shutdown`), at most
/// `SHUTDOWN_PATIENCE`, and then `exit` raises the request again with its
/// code (zero for one without), which goes through. A request while the
/// shutdown runs is held too (the exit is coming), and the shutdown runs
/// once (`ExitGate`).
///
/// Swift: `applicationShouldTerminate` answered `.terminateLater`, awaited
/// `AppController.shutdown()` and replied.
fn exit_request(
    code: Option<i32>,
    has_tray: impl FnOnce() -> bool,
    gate: &steno_services::app::ExitGate,
    shutdown: impl FnOnce() + Send + 'static,
    exit: impl FnOnce(i32) + Send + 'static,
) -> bool {
    if !exits_on(code, has_tray) {
        return false;
    }
    let code = code.unwrap_or(0);
    gate.exit_requested(
        steno_services::app::SHUTDOWN_PATIENCE,
        shutdown,
        move || exit(code),
    )
}

/// Whether an exit request ends the process. One with a code is the shell's
/// own (`AppHandle::exit` from Quit, the smoke's) and always does. One
/// without comes from the last window closing: with a tray the process
/// stays, as the Swift menu bar app stays when its window closes
/// (`applicationShouldTerminateAfterLastWindowClosed` in `StenoApp.swift`);
/// without one it ends. The tray is asked only for one without a code.
fn exits_on(code: Option<i32>, has_tray: impl FnOnce() -> bool) -> bool {
    code.is_some() || !has_tray()
}

/// How long the run loop may take to end on Linux once an exit went
/// through, before the process ends anyway (`end_within`).
#[cfg(target_os = "linux")]
const EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Calls `end` on a thread of its own `grace` from now, unless the process
/// has ended by then. On Linux an exit that went through ends this way at
/// the latest, with its code: the single-instance plugin releases its name
/// on the session bus at `RunEvent::Exit` (before the shell's own handler
/// runs) and waits for the bus's answer without a bound, so a frozen
/// session bus would hold the exit. The bus drops the name with the
/// connection anyway, and an exit goes through only once the shutdown
/// ended or ran out of patience, so ending the teardown early loses
/// nothing. An update's relaunch (`tauri::RESTART_EXIT_CODE`) is left to
/// the teardown, which relaunches at its end.
#[cfg(target_os = "linux")]
fn end_within(grace: std::time::Duration, end: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        std::thread::sleep(grace);
        end();
    });
}

/// Whether the session bus is named (`DBUS_SESSION_BUS_ADDRESS`): the
/// shell treats a session without one as having none.
fn session_bus_named() -> bool {
    std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
}

/// Whether the single-instance plugin can run: on Linux it holds a name on
/// the session bus and panics without one (a headless CI run under
/// `xvfb-run` has none), so it is skipped there; macOS and Windows need
/// nothing.
fn single_instance_available() -> bool {
    !cfg!(target_os = "linux") || session_bus_named()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A running Swift app refuses the start before the host is built; with
    /// none running the start goes on. Only the Swift app's bundle id is
    /// asked about.
    #[cfg(not(feature = "fixture-host"))]
    #[test]
    fn a_running_swift_app_refuses_the_start() {
        assert_eq!(
            refusal_before_build(|bundle_id| bundle_id == SWIFT_BUNDLE_ID),
            Some(Refusal::OlderSteno)
        );
        assert_eq!(refusal_before_build(|_| false), None);
        assert_eq!(Refusal::OlderSteno.title(), "An older Steno is running");
    }

    /// A host that cannot be built maps to its refusal: a store that cannot
    /// be opened is [`Refusal::Unavailable`], a database another process
    /// holds [`Refusal::DatabaseHeld`]. That `setup` shows it and exits
    /// with [`REFUSED_CODE`] is the Linux smoke's refusal run
    /// (`apps/desktop/scripts/smoke-linux.sh`).
    #[cfg(not(feature = "fixture-host"))]
    #[test]
    fn a_host_that_cannot_be_built_maps_to_its_refusal() {
        let unopened = host::ShellHostError::Build(steno_services::BuildError::Store(
            steno_core::StoreError::PendingMigration("v5".to_owned()),
        ));
        assert_eq!(Refusal::after(&unopened), Refusal::Unavailable);
        assert_eq!(Refusal::Unavailable.title(), "Steno could not start");
        let held = host::ShellHostError::Build(steno_services::BuildError::Lock(
            steno_core::DatabaseLockError::Held("steno.sqlite.lock".into()),
        ));
        assert_eq!(Refusal::after(&held), Refusal::DatabaseHeld);
    }

    /// The Mac's query answers false for a bundle id no app carries.
    #[cfg(all(target_os = "macos", not(feature = "fixture-host")))]
    #[test]
    fn an_app_that_is_not_running_reads_as_not_running() {
        assert!(!platform_app_running("com.nicolaischmid.steno.no-such-app"));
    }

    #[test]
    fn only_the_shells_own_exit_ends_the_process_while_a_tray_stands() {
        let asked = || panic!("the tray asked for an exit with a code");
        assert!(exits_on(Some(0), asked));
        assert!(exits_on(Some(1), asked));
        assert!(!exits_on(None, || true));
        // No tray: the last window closing ends the process.
        assert!(exits_on(None, || false));
    }

    /// The run loop's round trip: `exit` raises the request again, as
    /// `AppHandle::exit` raises `ExitRequested`, and reports whether that
    /// one went through.
    fn requested_again(
        gate: steno_services::app::ExitGate,
        steps: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        went_through: std::sync::mpsc::Sender<bool>,
    ) -> impl FnOnce(i32) + Send + 'static {
        move |code| {
            steps.lock().unwrap().push(format!("exit {code}"));
            let _ = went_through.send(exit_request(
                Some(code),
                || panic!("the tray asked for an exit with a code"),
                &gate,
                || panic!("a second shutdown"),
                |_| panic!("a second exit"),
            ));
        }
    }

    /// Quit (the tray's item, the menu bar's, `actions::quit`, a signal):
    /// the request is held, the shutdown runs once, then the process exits
    /// with Quit's code; nothing exits before the shutdown ended, and a
    /// second Quit meanwhile is held too, without a second shutdown.
    #[test]
    fn a_quit_runs_the_shutdown_once_and_holds_a_second_quit_until_it_ends() {
        use std::sync::{Arc, Mutex, atomic::AtomicUsize, atomic::Ordering};
        use std::time::Duration;

        let gate = steno_services::app::ExitGate::default();
        let steps = Arc::new(Mutex::new(Vec::<String>::new()));
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let (went_through, exit_seen) = std::sync::mpsc::channel();
        let shutdown = {
            let (steps, shutdowns) = (steps.clone(), shutdowns.clone());
            move || {
                shutdowns.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(100));
                steps.lock().unwrap().push("saved".to_owned());
            }
        };
        let held = !exit_request(
            Some(actions::QUIT_CODE),
            || panic!("the tray asked for an exit with a code"),
            &gate,
            shutdown,
            requested_again(gate.clone(), steps.clone(), went_through),
        );
        assert!(held, "Quit waits for the shutdown");
        assert!(
            !exit_request(
                Some(actions::QUIT_CODE),
                || true,
                &gate,
                || panic!("a second shutdown"),
                |_| panic!("a second exit"),
            ),
            "a second Quit while the shutdown runs is held"
        );
        assert!(
            exit_seen.recv_timeout(Duration::from_secs(5)).unwrap(),
            "the exit after the shutdown goes through"
        );
        assert_eq!(*steps.lock().unwrap(), ["saved", "exit 0"]);
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    }

    /// Closing main with no tray: the code-less request that ends the
    /// process is held the same way and exits with zero after the
    /// shutdown.
    #[test]
    fn a_close_without_a_tray_runs_the_shutdown_before_the_process_ends() {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        let gate = steno_services::app::ExitGate::default();
        let steps = Arc::new(Mutex::new(Vec::<String>::new()));
        let (went_through, exit_seen) = std::sync::mpsc::channel();
        let saved = steps.clone();
        assert!(!exit_request(
            None,
            || false,
            &gate,
            move || saved.lock().unwrap().push("saved".to_owned()),
            requested_again(gate.clone(), steps.clone(), went_through),
        ));
        assert!(exit_seen.recv_timeout(Duration::from_secs(5)).unwrap());
        assert_eq!(*steps.lock().unwrap(), ["saved", "exit 0"]);
    }

    /// A close behind a tray ends nothing, so it runs nothing.
    #[test]
    fn a_close_behind_a_tray_runs_no_shutdown() {
        let gate = steno_services::app::ExitGate::default();
        assert!(!exit_request(
            None,
            || true,
            &gate,
            || panic!("a shutdown"),
            |_| panic!("an exit"),
        ));
        // The gate is still open: the next exit runs the shutdown.
        let (shut_down, seen) = std::sync::mpsc::channel();
        assert!(!exit_request(
            Some(actions::QUIT_CODE),
            || true,
            &gate,
            move || shut_down.send(()).unwrap(),
            |_| {},
        ));
        seen.recv_timeout(std::time::Duration::from_secs(5))
            .expect("the shutdown ran");
    }

    /// A tray stands when it was built and a host or a smoke run shows it;
    /// the host is asked only when that decides.
    #[test]
    fn a_tray_stands_when_built_and_shown() {
        for (built, smoke, host, stands, asks) in [
            (true, false, true, true, true),
            (true, false, false, false, true),
            (true, true, false, true, false),
            (true, true, true, true, false),
            (false, false, true, false, false),
            (false, true, true, false, false),
        ] {
            let asked = std::cell::Cell::new(false);
            let result = tray_stands(built, smoke, || {
                asked.set(true);
                host
            });
            assert_eq!(result, stands, "{built} {smoke} {host}");
            assert_eq!(asked.get(), asks, "{built} {smoke} {host}");
        }
    }

    /// With a tray, closing main hides it and the process stays; without
    /// one, main is destroyed and the process ends with it, whatever else
    /// (a hidden panel) is still around. The other windows just close.
    #[test]
    fn main_hides_behind_a_tray_and_ends_the_process_without_one() {
        assert!(hides_on_close("main", true));
        assert!(!hides_on_close("main", false));
        assert!(!exits_when_destroyed("main", || true));
        assert!(exits_when_destroyed("main", || false));
        for label in ["settings", "onboarding", "bubble", "prompt"] {
            for tray in [true, false] {
                assert!(!hides_on_close(label, tray), "{label}");
                assert!(
                    !exits_when_destroyed(label, || panic!("asked for {label}")),
                    "{label}"
                );
            }
        }
    }

    /// Only a second SIGTERM or a second SIGINT ends the process at once;
    /// every other exit signal asks for Quit, and a SIGHUP, the one systemd
    /// sends right after a session scope's SIGTERM, never forces the exit.
    #[cfg(unix)]
    #[test]
    fn a_second_sigterm_or_sigint_forces_the_exit_and_a_sighup_never_does() {
        let mut seen = Vec::new();
        assert_eq!(forced_exit(ExitSignal::TERMINATE, &mut seen), None);
        assert_eq!(forced_exit(ExitSignal::HANGUP, &mut seen), None);
        assert_eq!(forced_exit(ExitSignal::HANGUP, &mut seen), None);
        assert_eq!(forced_exit(ExitSignal::INTERRUPT, &mut seen), None);
        assert_eq!(forced_exit(ExitSignal::INTERRUPT, &mut seen), Some(130));
        assert_eq!(forced_exit(ExitSignal::TERMINATE, &mut seen), Some(143));

        let mut seen = Vec::new();
        assert_eq!(forced_exit(ExitSignal::INTERRUPT, &mut seen), None);
        assert_eq!(forced_exit(ExitSignal::TERMINATE, &mut seen), None);
        assert_eq!(forced_exit(ExitSignal::TERMINATE, &mut seen), Some(143));
    }

    /// A signal quits the pipeline, then asks for Quit, with `seen`
    /// unlocked for both; a second SIGTERM does neither and forces the exit.
    #[cfg(unix)]
    #[test]
    fn a_signal_quits_the_pipeline_before_it_asks_for_quit() {
        let seen = std::sync::Mutex::new(Vec::new());
        let steps = std::cell::RefCell::new(Vec::new());
        let step = |name: &'static str| {
            assert!(seen.try_lock().is_ok(), "{name} ran under the guard");
            steps.borrow_mut().push(name);
        };
        let first = on_exit_signal(
            ExitSignal::TERMINATE,
            &seen,
            || step("quit the pipeline"),
            || step("ask for Quit"),
        );
        assert_eq!(first, None);
        assert_eq!(*steps.borrow(), ["quit the pipeline", "ask for Quit"]);

        let second = on_exit_signal(
            ExitSignal::TERMINATE,
            &seen,
            || step("quit the pipeline"),
            || step("ask for Quit"),
        );
        assert_eq!(second, Some(143));
        assert_eq!(steps.borrow().len(), 2);
    }

    /// The exit signals are SIGTERM, SIGINT and SIGHUP, and only the first
    /// two force a second time, with 128 plus their number.
    #[cfg(unix)]
    #[test]
    fn the_exit_signals_are_sigterm_sigint_and_sighup() {
        let listened: Vec<_> = ExitSignal::ALL
            .iter()
            .map(|signal| (signal.kind.as_raw_value(), signal.forced_code))
            .collect();
        assert_eq!(
            listened,
            [
                (libc::SIGTERM, Some(143)),
                (libc::SIGINT, Some(130)),
                (libc::SIGHUP, None)
            ]
        );
    }

    /// A signal ignored at launch (`nohup`, a background job's SIGINT)
    /// reads as ignored; a default one does not, nor does one whose
    /// disposition cannot be read (no such signal). The disposition is read
    /// as it is: SIGUSR2, which nothing here uses, ignored for the test.
    #[cfg(unix)]
    #[test]
    fn a_signal_ignored_at_launch_reads_as_ignored() {
        assert!(!ignored(-1));

        // SAFETY: SIGUSR2 has no handler in this binary; it is set back.
        let before = unsafe { libc::signal(libc::SIGUSR2, libc::SIG_IGN) };
        let when_ignored = ignored(libc::SIGUSR2);
        unsafe { libc::signal(libc::SIGUSR2, libc::SIG_DFL) };
        let when_default = ignored(libc::SIGUSR2);
        unsafe { libc::signal(libc::SIGUSR2, before) };
        assert!(when_ignored);
        assert!(!when_default);
    }

    /// Set in the copy of this test binary the stderr test runs.
    #[cfg(unix)]
    const STDERR_CHILD: &str = "STENO_TEST_STDERR_CHILD";

    /// The child writes a line to a stderr nobody reads (a pipe with no
    /// reader fails each write, as a closed terminal does), so the line is
    /// lost; the child must still pass, not panic. `--nocapture`, or the
    /// test harness would catch the line. Unix only, as the services' test
    /// of their log lines.
    #[cfg(unix)]
    #[test]
    fn a_line_to_a_closed_stderr_is_dropped_without_a_panic() {
        if std::env::var_os(STDERR_CHILD).is_some() {
            stderr_line!("[steno-desktop] a line nobody reads");
            return;
        }
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::a_line_to_a_closed_stderr_is_dropped_without_a_panic",
                "--nocapture",
            ])
            .env(STDERR_CHILD, "1")
            .stdout(std::process::Stdio::null())
            .stderr(writer)
            .status()
            .unwrap();
        assert!(status.success(), "{status}");
    }

    /// An exit that went through ends the process `EXIT_GRACE` later at the
    /// latest, never sooner.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_exit_ends_once_the_grace_has_passed() {
        let grace = std::time::Duration::from_millis(100);
        let started = std::time::Instant::now();
        let (ended, seen) = std::sync::mpsc::channel();
        end_within(grace, move || ended.send(started.elapsed()).unwrap());
        let after = seen
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("ended");
        assert!(after >= grace, "{after:?}");
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
