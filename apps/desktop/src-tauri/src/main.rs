//! The Tauri shell: three windows and two floating panels around the web UI
//! in `apps/macos/web`, a tray icon, and the `bridge_call` command the
//! page's Tauri transport talks to. It holds no logic: the bridge is
//! answered by `steno_host::Host` over the services graph (`host`), or by
//! the recorded fixtures with the `fixture-host` feature.
//!
//! Every exit runs `App::shutdown` first, at most `SHUTDOWN_PATIENCE`, over
//! one `ExitGate`, so a recording in progress is saved: Quit, the last
//! close without a tray, exit signals, and the run loop's last event alike.
//! A second SIGTERM or SIGINT ends the process at once, unsaved.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// The fixture host leaves the real host's seams unused.
#![cfg_attr(feature = "fixture-host", allow(dead_code))]

/// `eprintln!` for the shell's own lines, minus its panic: once the
/// terminal the app started from has closed, the line is dropped instead.
macro_rules! stderr_line {
    ($($line:tt)*) => {
        $crate::write_stderr_line(format_args!($($line)*))
    };
}

fn write_stderr_line(line: std::fmt::Arguments<'_>) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "{line}");
}

mod actions;
#[cfg(target_os = "linux")]
mod appimage;
mod autostart;
mod bridge;
#[cfg(target_os = "linux")]
mod dbus;
mod deep_links;
mod dialogs;
#[cfg(target_os = "linux")]
mod display;
#[cfg(target_os = "linux")]
mod display_lost;
#[cfg(feature = "fixture-host")]
mod fixtures;
mod host;
mod identifier;
#[cfg(target_os = "macos")]
mod menu;
mod navigation;
#[cfg(target_os = "linux")]
mod own_scope;
mod packaged;
mod panel_anchor;
mod panel_geometry;
mod panels;
mod permissions;
mod platform;
mod recording;
#[cfg(target_os = "linux")]
mod session_end;
mod smoke;
#[cfg(target_os = "linux")]
mod stop_timeout;
mod tray;
#[cfg(target_os = "linux")]
mod tray_host;
mod updater;
mod windows;

use tauri::Manager;

use crate::windows::BridgeWindow;

fn main() {
    steno_services::log_to_stderr(steno_services::LOG_FILTER);
    // After the log output: a panic leaves a file under the support
    // directory, since an app opened from the Finder or at login has no
    // stderr anyone reads.
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
    // The runtime the services graph runs on, beside Tauri's own. Leaked:
    // dropping a runtime waits without a bound for every blocking task on
    // it, and Tauri drops whatever owns it before the process exits.
    let runtime: &'static tokio::runtime::Runtime = Box::leak(Box::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime"),
    ));
    // A host call on the event loop's thread fails a debug build, since it
    // would freeze every window and the tray.
    steno_host::mark_ui_thread();
    let mut builder = tauri::Builder::default();
    // First, so a second instance exits before it builds anything and
    // hands its arguments to this one.
    if single_instance_available() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if host::is_running(app) {
                actions::open(app, windows::BridgeWindow::Main);
            }
        }));
    }
    // macOS registers the login item with `SMAppService` (`autostart`).
    #[cfg(not(target_os = "macos"))]
    {
        builder = builder.plugin(autostart::plugin());
    }
    builder = builder
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(updater::plugin())
        .plugin(platform::plugin());
    #[cfg(target_os = "macos")]
    {
        // The shell's own menu bar: Tauri's default one ends in a Quit that
        // terminates without `ExitRequested`.
        builder = builder
            .plugin(tauri_nspanel::init())
            .enable_macos_default_menu(false)
            .menu(menu::build);
    }
    let app = builder
        // One handler for every menu: the tray's and the menu bar's.
        .on_menu_event(|app, event| {
            if host::is_running(app) {
                actions::on_menu_event(app, &event);
            }
        })
        .manage(smoke::Smoke::default())
        .manage(panels::Panels::default())
        .manage(windows::Pages::default())
        .manage(host::Lane::start())
        .manage(windows::Kept::default())
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
/// the host asks for it, and runs the launch sequence. False when this app
/// refuses to start (`refuse_to_start`), with no host, tray or window.
fn setup(
    handle: &tauri::AppHandle,
    runtime: &'static tokio::runtime::Runtime,
) -> Result<bool, Box<dyn std::error::Error>> {
    // Before the first window starts WebKit's processes, so they follow the
    // app into its scope.
    #[cfg(target_os = "linux")]
    own_scope::take_own_scopes();
    handle.manage(std::sync::Arc::new(updater::ShellUpdates::new(
        handle.clone(),
    )));
    #[cfg(not(feature = "fixture-host"))]
    if let Some(refusal) = refusal_before_build(platform_app_running) {
        refuse_to_start(
            handle,
            Refused::plain(refusal, format!("{SWIFT_BUNDLE_ID} is running")),
        );
        return Ok(false);
    }
    #[cfg(not(feature = "fixture-host"))]
    let host = match host::Host::real(handle, runtime) {
        Ok(host) => host,
        Err(error) => {
            // The whole error, paths included, only at debug level.
            tracing::debug!("not starting: {error}");
            refuse_to_start(handle, Refused::after(&error));
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
    #[cfg(target_os = "linux")]
    autostart::remove_earlier_entry(handle);
    #[cfg(target_os = "macos")]
    autostart::remove_earlier_agent();
    host::host(handle).launch(handle, runtime);
    // The launch may have registered the login item.
    tray::note_login_item(handle, autostart::status(handle).is_on());
    #[cfg(target_os = "linux")]
    autostart::sync_at_launch(handle);
    #[cfg(unix)]
    exit_on_signals(handle, runtime);
    Ok(true)
}

const REFUSED_CODE: i32 = 3;

/// How long a refused app waits for its dialog to be closed before it ends
/// anyway.
const REFUSED_PATIENCE: std::time::Duration = std::time::Duration::from_secs(60);

/// The Swift app's bundle id. It takes no database lock, so this app looks
/// for it by name.
const SWIFT_BUNDLE_ID: &str = "uno.schmid.steno.mac";

/// Why this app refuses to start, with its dialog's title and text. A
/// refusal the user can fix names what to fix, then "open Steno again".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The Swift app runs; it takes no lock to say so.
    OlderSteno,
    /// Another process holds the database: another app or a `steno`
    /// command.
    DatabaseHeld,
    /// A stored setting, or the settings together, cannot be read. The
    /// dialog names the key and the database, never a value.
    UnreadableSetting,
    /// The host could not be built for another reason (the database or the
    /// support directory). Nothing in the database is lost.
    Unavailable,
}

impl Refusal {
    fn title(self) -> &'static str {
        match self {
            Refusal::OlderSteno => "An older Steno is running",
            Refusal::DatabaseHeld => "Steno is already running",
            Refusal::UnreadableSetting | Refusal::Unavailable => "Steno could not start",
        }
    }

    fn text(self) -> &'static str {
        match self {
            Refusal::OlderSteno => "Quit the older Steno app first, then open Steno again.",
            Refusal::DatabaseHeld => {
                "Another Steno is already open, or a steno command is running in a terminal. \
                 Quit it, then open Steno again."
            }
            Refusal::UnreadableSetting => {
                "Steno could not read its settings. Your meetings are safe."
            }
            Refusal::Unavailable => {
                "Steno could not open your meetings. They are safe. Open Steno again."
            }
        }
    }
}

/// What a refused start shows and logs: the refusal, the log line, which
/// names no path or value, and the dialog's text, which may name the
/// database.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refused {
    refusal: Refusal,
    log_line: String,
    message: String,
}

impl Refused {
    fn plain(refusal: Refusal, log_line: String) -> Self {
        Self {
            refusal,
            log_line,
            message: refusal.text().to_owned(),
        }
    }

    /// The refusal for a host that could not be built:
    /// [`Refusal::DatabaseHeld`], [`Refusal::UnreadableSetting`], else
    /// [`Refusal::Unavailable`].
    #[cfg(not(feature = "fixture-host"))]
    fn after(error: &host::ShellHostError) -> Self {
        use steno_core::StoreError;

        let log_line = refusal_log_line(error);
        if error.is_database_held() {
            return Self::plain(Refusal::DatabaseHeld, log_line);
        }
        let Some((source, database)) = error.unreadable_settings() else {
            return Self::plain(Refusal::Unavailable, log_line);
        };
        let database = database.display();
        let detail = match source {
            StoreError::UndecodableSetting { key, problem } => format!(
                "The setting {key} {problem}. It is stored in the database at {database}. \
                 Steno has not changed it: fix or remove it there, then open Steno again."
            ),
            StoreError::UndecodableSettings => format!(
                "The stored settings cannot be read together, though each reads on its own. \
                 They are stored in the database at {database}. Steno has not changed them: \
                 check the settings there, then open Steno again."
            ),
            // Not built today; named so a new variant must choose.
            StoreError::Sqlite(_)
            | StoreError::Json(_)
            | StoreError::Io(_)
            | StoreError::MeetingNotFound(_)
            | StoreError::SpeakerNotFound(_)
            | StoreError::PersonNotFound(_)
            | StoreError::SpeakersInDifferentMeetings(..)
            | StoreError::BlankPersonName
            | StoreError::MeetingBusy(..)
            | StoreError::NotRecording(..)
            | StoreError::MeetingExists(_)
            | StoreError::ReceiptOfAnotherUpload(_)
            | StoreError::PendingMigration(_)
            | StoreError::ForeignKeyViolations { .. }
            | StoreError::JournalModeNotWal(_)
            | StoreError::OwnReaderOpen => {
                return Self::plain(Refusal::Unavailable, log_line);
            }
        };
        let refusal = Refusal::UnreadableSetting;
        Self {
            refusal,
            log_line,
            message: format!("{}\n\n{detail}", refusal.text()),
        }
    }
}

/// The error-level log line for a host that could not be built: what went
/// wrong, never a path or a value (the dialog may name the database, the
/// log may not).
#[cfg(not(feature = "fixture-host"))]
fn refusal_log_line(error: &host::ShellHostError) -> String {
    use host::ShellHostError;
    use steno_core::DatabaseLockError;
    use steno_services::BuildError;

    match error {
        ShellHostError::SupportDirectory(error) => {
            format!(
                "the support directory could not be created ({})",
                error.kind()
            )
        }
        ShellHostError::Build(BuildError::Lock(DatabaseLockError::Held(_))) => {
            "another Steno holds the database".to_owned()
        }
        ShellHostError::Build(BuildError::Lock(DatabaseLockError::Unsupported {
            source, ..
        })) => format!(
            "the database cannot be locked on this filesystem ({})",
            source.kind()
        ),
        ShellHostError::Build(BuildError::Lock(DatabaseLockError::Io { source, .. })) => {
            format!("the database could not be locked ({})", source.kind())
        }
        ShellHostError::Build(BuildError::DatabaseFolder(error)) => {
            format!(
                "the database folder could not be created ({})",
                error.kind()
            )
        }
        ShellHostError::Build(BuildError::Setting { source, .. }) => source.to_string(),
        // SQLite's result code names no path or value.
        ShellHostError::Build(BuildError::Store(error)) => match error.sqlite_code() {
            Some(code) => format!("the database could not be opened or read (SQLite: {code:?})"),
            None => "the database could not be opened or read".to_owned(),
        },
        ShellHostError::Host(_) => "the host could not be built".to_owned(),
    }
}

/// The refusal due before the host is built: [`Refusal::OlderSteno`] while
/// the Swift app runs, as `running` answers.
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

/// Says why this app does not start, logs it without a path or value, and
/// ends with [`REFUSED_CODE`] once the dialog is closed, before it opens a
/// window. Should the dialog never close, the process ends after
/// [`REFUSED_PATIENCE`] (or the smoke run's wait) all the same.
fn refuse_to_start(handle: &tauri::AppHandle, refused: Refused) {
    use tauri_plugin_dialog::{DialogExt as _, MessageDialogButtons, MessageDialogKind};

    let Refused {
        refusal,
        log_line,
        message,
    } = refused;
    stderr_line!("[steno-desktop] not starting: {log_line}");
    tracing::error!("not starting: {log_line}");
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
        .message(message)
        .title(refusal.title())
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::Ok)
        .show(move |_| app.exit(REFUSED_CODE));
}

/// A signal that asks for the exit Quit asks for.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExitSignal {
    kind: tokio::signal::unix::SignalKind,
    name: &'static str,
    /// The code a second one ends the process with, 128 plus the signal's
    /// number; none for SIGHUP, which never forces the exit.
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

/// Whether the signal `number` is ignored now, as `nohup` leaves it; false
/// when unreadable. The app does not listen for such a signal, so it stays
/// ignored.
#[cfg(unix)]
fn ignored(number: libc::c_int) -> bool {
    // SAFETY: an all-zero `sigaction` is a valid value, and a null new
    // action makes the call read the disposition without changing it.
    let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
    let read = unsafe { libc::sigaction(number, std::ptr::null(), &raw mut current) };
    read == 0 && current.sa_sigaction == libc::SIG_IGN
}

/// What `signal` does after the exit signals already `seen`: the code to
/// end the process with at once, or none to ask for the Quit exit.
#[cfg(unix)]
fn forced_exit(signal: ExitSignal, seen: &mut Vec<ExitSignal>) -> Option<i32> {
    if seen.contains(&signal) {
        return signal.forced_code;
    }
    seen.push(signal);
    None
}

/// One arrival of `signal`: the code to end the process with at once, or
/// none once it has quit the pipeline and then asked for Quit, so the
/// pipeline is quit before the request waits for the main thread.
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
/// shutdown runs first. Each quits the pipeline on the blocking pool and
/// then asks for the exit, so a job stays resumable even while the main
/// thread is busy. A second SIGTERM or SIGINT ends the process at once,
/// unsaved; a SIGHUP never does, so the one that follows a session scope's
/// SIGTERM still saves. A signal inherited ignored stays ignored.
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
                // Not waited for, so the next arrival is received while the
                // quit syncs the disk.
                let (app, seen) = (app.clone(), seen.clone());
                tokio::task::spawn_blocking(move || {
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
                });
            }
        });
    }
}

/// Runs the shutdown for an exit no request held and returns once it ended
/// (at most `SHUTDOWN_PATIENCE`), joining one already running; at once when
/// it already ran. Called right before the process ends, so it writes out
/// the queued log lines too.
fn shut_down_before_exit(app: &tauri::AppHandle) {
    app.state::<steno_services::app::ExitGate>()
        .exiting(steno_services::app::SHUTDOWN_PATIENCE, exit_action(app));
    steno_services::flush_logs();
}

/// The shutdown before an update's relaunch: on Linux it marks the exit as
/// a relaunch first, so the login item stays for the next process.
fn shut_down_for_relaunch(app: &tauri::AppHandle) {
    #[cfg(target_os = "linux")]
    autostart::relaunching();
    shut_down_before_exit(app);
}

/// The save before an end no exit request announced, on Linux: the
/// pipeline quits first, then the shutdown runs on this thread's behalf.
#[cfg(target_os = "linux")]
fn save_before_end(app: &tauri::AppHandle) {
    host::host(app).quit_pipeline();
    shut_down_before_exit(app);
}

/// The onboarding window closed: the host counts its pages as seen, and a
/// smoke run counts the close.
fn onboarding_closed(app: &tauri::AppHandle) {
    app.state::<smoke::Smoke>().note_onboarding_closed();
    count_onboarding_seen(app.clone());
}

/// `Host::onboarding_window_closed`, queued on the shell's ordered lane,
/// since it reads the store.
fn count_onboarding_seen<H: host::ShellHost>(host: H) {
    host::call_in_order(host, "the onboarding window's close", |host| {
        host.onboarding_window_closed();
        Ok(())
    });
}

/// What every exit runs once: the host's shutdown, then a `warn` line with
/// how long it took, so a machine's log shows the save against the waits
/// it must fit in. Then, on Linux, the login item's exit step, only once
/// the save is over; last the panels' anchor, which waits for a slow disk.
fn exit_action(app: &tauri::AppHandle) -> impl FnOnce() + Send + 'static {
    let shutdown = host::host(app).shutdown_action();
    let app = app.clone();
    timed_then(shutdown, move || {
        #[cfg(target_os = "linux")]
        autostart::at_exit(&app, autostart::relaunching_now());
        panels::flush_anchor(&app);
    })
}

fn timed_then(
    shutdown: impl FnOnce() + Send + 'static,
    after: impl FnOnce() + Send + 'static,
) -> impl FnOnce() + Send + 'static {
    move || {
        let started = std::time::Instant::now();
        shutdown();
        tracing::warn!(elapsed = ?started.elapsed(), "the shutdown ended");
        after();
    }
}

/// One turn of the run loop; nothing for an app that refused to start.
fn on_event(app: &tauri::AppHandle, event: tauri::RunEvent) {
    if !host::is_running(app) {
        return;
    }
    match event {
        // Quit, an exit signal, the smoke's exit, the last window closing
        // with no tray: the shutdown runs first.
        tauri::RunEvent::ExitRequested { code, api, .. } => {
            let handle = app.clone();
            if exit_request(
                code,
                || tray_at_close(app),
                &app.state::<steno_services::app::ExitGate>(),
                exit_action(app),
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
        // The run loop's last event. The Dock's Quit, a logout or shutdown
        // on macOS and a logoff or shutdown on Windows raise no request, so
        // the shutdown runs here.
        tauri::RunEvent::Exit => shut_down_before_exit(app),
        // The main window closes: hidden while a tray can bring it back,
        // destroyed otherwise; `has_tray` is asked once per close.
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } if label == BridgeWindow::Main.as_str() => {
            let tray = has_tray(app);
            app.state::<TrayAtClose>().note(tray);
            if !tray {
                tracing::warn!(
                    "the main window closed with no tray to bring it back; \
                     quitting, which saves a recording in progress first"
                );
            }
            if hides_on_close(&label, tray) {
                api.prevent_close();
                if let Some(main) = app.get_webview_window(&label)
                    && let Err(error) = main.hide()
                {
                    stderr_line!("[steno-desktop] hiding the main window failed: {error}");
                }
            }
        }
        // Settings or onboarding closes on Linux: kept, not destroyed; one
        // that could not be kept is destroyed.
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } if windows::keeps_on_close(&label) => {
            api.prevent_close();
            let Some(window) = app.get_webview_window(&label) else {
                return;
            };
            match windows::keep(app, &window) {
                Ok(true) if label == BridgeWindow::Onboarding.as_str() => onboarding_closed(app),
                Ok(_) => {}
                Err(error) => {
                    stderr_line!(
                        "[steno-desktop] keeping the {label} window failed, so it closes: {error}"
                    );
                    if let Err(error) = window.destroy() {
                        stderr_line!("[steno-desktop] closing the {label} window failed: {error}");
                    }
                }
            }
        }
        // A window is gone; a destroyed main window with no tray ends the
        // process.
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::Destroyed,
            ..
        } => {
            app.state::<windows::Pages>().gone(&label);
            let kept = app.state::<windows::Kept>().forget(&label);
            if label == BridgeWindow::Onboarding.as_str() && !kept {
                onboarding_closed(app);
            }
            if exits_when_destroyed(&label, || tray_at_close(app)) {
                actions::quit(app);
            }
        }
        // A panel the user dragged: its anchor follows.
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::Moved(position),
            ..
        } => {
            if let Some(panel) = panels::Panel::from_label(&label) {
                panels::moved(app, panel, position);
            }
        }
        // The Dock icon was clicked with no window open: main comes back.
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen {
            has_visible_windows: false,
            ..
        } => actions::open(app, windows::BridgeWindow::Main),
        _ => {}
    }
}

/// Builds the tray. No tray is not fatal: closing main then ends the
/// process. On Linux the tray crate panics without
/// libayatana-appindicator, so the panic is caught here.
fn build_tray(app: &tauri::AppHandle) {
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tray::build(app)));
    match built {
        Ok(Ok(())) => {
            app.state::<smoke::Smoke>().note_tray();
            #[cfg(target_os = "linux")]
            tray_host::follow();
        }
        Ok(Err(error)) => stderr_line!("[steno-desktop] the tray could not be built: {error}"),
        Err(_) => {
            stderr_line!(
                "[steno-desktop] the tray could not be built: the tray library is missing"
            );
        }
    }
}

/// Whether a tray stands; `tray::build` manages `Tray` on success.
fn has_tray(app: &tauri::AppHandle) -> bool {
    tray_stands(
        app.try_state::<tray::Tray>().is_some(),
        app.state::<smoke::Smoke>().is_armed(),
        tray::has_host(),
    )
}

/// Whether a tray stands: it was `built` and a `host` shows it, or a smoke
/// run stands in for the host Xvfb lacks.
fn tray_stands(built: bool, smoke: bool, host: bool) -> bool {
    built && (smoke || host)
}

/// What `has_tray` said when main last closed, so the events that follow
/// a close decide as it did.
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

fn tray_at_close(app: &tauri::AppHandle) -> bool {
    app.state::<TrayAtClose>()
        .noted()
        .unwrap_or_else(|| has_tray(app))
}

/// Whether closing the window of `label` hides it instead: main while a
/// tray can bring it back.
fn hides_on_close(label: &str, has_tray: bool) -> bool {
    label == BridgeWindow::Main.as_str() && has_tray
}

/// Whether the window of `label` being destroyed ends the process: main
/// with no tray, since nothing else reaches the app (the hidden panels
/// would otherwise keep it lingering invisibly).
fn exits_when_destroyed(label: &str, has_tray: impl FnOnce() -> bool) -> bool {
    label == BridgeWindow::Main.as_str() && !has_tray()
}

/// What an exit request does; true lets it through now. The first one
/// that ends the process is held while `shutdown` runs on a thread of its
/// own, then `exit` raises it again with its code, which goes through. A
/// request during the shutdown is held too; the shutdown runs once.
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

/// Whether an exit request ends the process: one with a code is the
/// shell's own and always does; one without (the last window closing) only
/// when no tray stands.
fn exits_on(code: Option<i32>, has_tray: impl FnOnce() -> bool) -> bool {
    code.is_some() || !has_tray()
}

/// How long the run loop may take to end on Linux once an exit went
/// through.
#[cfg(target_os = "linux")]
const EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Calls `end` on a thread of its own `grace` from now, unless the process
/// has ended. On Linux the single-instance plugin waits without a bound
/// for the session bus at `RunEvent::Exit`, so a frozen bus would hold the
/// exit; ending early loses nothing. An update's relaunch is left to the
/// teardown.
#[cfg(target_os = "linux")]
fn end_within(grace: std::time::Duration, end: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        std::thread::sleep(grace);
        end();
    });
}

/// Whether the session bus is named (`DBUS_SESSION_BUS_ADDRESS`).
fn session_bus_named() -> bool {
    std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
}

/// Whether the single-instance plugin can run: on Linux it panics without
/// a session bus, so it is skipped there.
fn single_instance_available() -> bool {
    !cfg!(target_os = "linux") || session_bus_named()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exit turns the Linux login item off before it waits for the
    /// panels' anchor.
    #[test]
    fn the_exit_flushes_the_anchor_after_the_login_item() {
        let main = include_str!("main.rs").replace("\r\n", "\n");
        let start = main.find("\nfn exit_action(").unwrap();
        let end = start + main[start..].find("\n}\n").unwrap();
        let exit = &main[start..end];
        let login_item = exit
            .find("autostart::at_exit(&app, autostart::relaunching_now());")
            .expect("the exit turns the Linux login item off");
        let flush = exit
            .find("panels::flush_anchor(&app);")
            .expect("the exit flushes the anchor");
        assert!(login_item < flush);
    }

    /// A running Swift app refuses the start before the host is built.
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

    /// A host that cannot be built maps to its refusal, each with its text
    /// alone.
    #[cfg(not(feature = "fixture-host"))]
    #[test]
    fn a_host_that_cannot_be_built_maps_to_its_refusal() {
        let unopened = Refused::after(&host::ShellHostError::Build(
            steno_services::BuildError::Store(steno_core::StoreError::PendingMigration(
                "v5".to_owned(),
            )),
        ));
        assert_eq!(unopened.refusal, Refusal::Unavailable);
        assert_eq!(unopened.message, Refusal::Unavailable.text());
        assert_eq!(Refusal::Unavailable.title(), "Steno could not start");
        let held = Refused::after(&host::ShellHostError::Build(
            steno_services::BuildError::Lock(steno_core::DatabaseLockError::Held(
                "steno.sqlite.lock".into(),
            )),
        ));
        assert_eq!(held.refusal, Refusal::DatabaseHeld);
        assert_eq!(held.message, Refusal::DatabaseHeld.text());
    }

    /// Unreadable stored settings say which one, where, and what to do.
    #[cfg(not(feature = "fixture-host"))]
    #[test]
    fn an_unreadable_setting_names_itself_in_the_dialog() {
        let refused = |source| {
            Refused::after(&host::ShellHostError::Build(
                steno_services::BuildError::Setting {
                    database: "/Users/n/Steno/steno.sqlite".into(),
                    source,
                },
            ))
        };
        let one = refused(steno_core::StoreError::UndecodableSetting {
            key: "llmBaseURL".to_owned(),
            problem: steno_core::SettingProblem::NotJson,
        });
        assert_eq!(one.refusal, Refusal::UnreadableSetting);
        assert_eq!(Refusal::UnreadableSetting.title(), "Steno could not start");
        assert_eq!(
            one.message,
            "Steno could not read its settings. Your meetings are safe.\n\n\
             The setting llmBaseURL is not in a form Steno can read. It is stored in the \
             database at /Users/n/Steno/steno.sqlite. Steno has not changed it: fix or \
             remove it there, then open Steno again."
        );
        let together = refused(steno_core::StoreError::UndecodableSettings);
        assert_eq!(together.refusal, Refusal::UnreadableSetting);
        assert_eq!(
            together.message,
            "Steno could not read its settings. Your meetings are safe.\n\n\
             The stored settings cannot be read together, though each reads on its own. \
             They are stored in the database at /Users/n/Steno/steno.sqlite. Steno has not \
             changed them: check the settings there, then open Steno again."
        );
        let other = refused(steno_core::StoreError::BlankPersonName);
        assert_eq!(
            other,
            Refused::plain(Refusal::Unavailable, "a person needs a name".to_owned())
        );
    }

    /// The log line of every refusal after a failed build names no path.
    #[cfg(not(feature = "fixture-host"))]
    #[test]
    fn a_refusal_logs_no_path() {
        use host::ShellHostError::{Build, SupportDirectory};
        use steno_core::{DatabaseLockError, SettingProblem, StoreError};
        use steno_services::BuildError;

        let database = "/Users/n/Library/Steno/steno.sqlite";
        let setting = Refused::after(&Build(BuildError::Setting {
            database: database.into(),
            source: StoreError::UndecodableSetting {
                key: "obsidian".to_owned(),
                problem: SettingProblem::WrongShape,
            },
        }));
        assert_eq!(
            setting.log_line,
            "the stored setting obsidian holds a value this version of Steno does not understand"
        );
        assert!(setting.message.contains(database), "{}", setting.message);
        let together = Refused::after(&Build(BuildError::Setting {
            database: database.into(),
            source: StoreError::UndecodableSettings,
        }));
        assert_eq!(
            together.log_line,
            "the stored settings cannot be read together"
        );
        assert!(together.message.contains(database), "{}", together.message);
        let lock = "/Users/n/Library/Steno/steno.lock";
        let held = Refused::after(&Build(BuildError::Lock(DatabaseLockError::Held(
            lock.into(),
        ))));
        assert_eq!(held.log_line, "another Steno holds the database");
        let scratch = tempfile::tempdir().unwrap();
        let corrupt = scratch.path().join("steno.sqlite");
        std::fs::write(&corrupt, "not a database\n").unwrap();
        let Err(error) = steno_core::Store::open(&corrupt) else {
            panic!("a file that is not a database opened");
        };
        let corrupt = Refused::after(&Build(BuildError::Store(error)));
        assert_eq!(
            corrupt.log_line,
            "the database could not be opened or read (SQLite: NotADatabase)"
        );
        let io = || std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        for refused in [
            setting,
            together,
            held,
            corrupt,
            Refused::after(&Build(BuildError::Lock(DatabaseLockError::Unsupported {
                path: lock.into(),
                source: io(),
            }))),
            Refused::after(&Build(BuildError::Lock(DatabaseLockError::Io {
                path: lock.into(),
                source: io(),
            }))),
            Refused::after(&Build(BuildError::Store(StoreError::PendingMigration(
                "/Users/n/v9".to_owned(),
            )))),
            Refused::after(&Build(BuildError::DatabaseFolder(io()))),
            Refused::after(&SupportDirectory(io())),
        ] {
            assert!(!refused.log_line.contains('/'), "{}", refused.log_line);
        }
    }

    /// The Mac's query answers false for a bundle id no app carries.
    #[cfg(all(target_os = "macos", not(feature = "fixture-host")))]
    #[test]
    fn an_app_that_is_not_running_reads_as_not_running() {
        assert!(!platform_app_running("com.nicolaischmid.steno.no-such-app"));
    }

    /// What runs after the shutdown runs only once the save is over.
    #[test]
    fn the_exit_action_runs_its_after_step_once_the_shutdown_ended() {
        let steps = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (shutdown, after) = (steps.clone(), steps.clone());
        timed_then(
            move || shutdown.lock().unwrap().push("shutdown"),
            move || after.lock().unwrap().push("after"),
        )();
        assert_eq!(*steps.lock().unwrap(), ["shutdown", "after"]);
    }

    #[test]
    fn only_the_shells_own_exit_ends_the_process_while_a_tray_stands() {
        let asked = || panic!("the tray asked for an exit with a code");
        assert!(exits_on(Some(0), asked));
        assert!(exits_on(Some(1), asked));
        assert!(!exits_on(None, || true));
        assert!(exits_on(None, || false));
    }

    /// The run loop's round trip: `exit` raises the request again and
    /// reports whether that one went through.
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

    /// Quit: the request is held, the shutdown runs once, then the process
    /// exits with Quit's code; a second Quit meanwhile is held too.
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

    /// Closing main with no tray is held the same way and exits with zero.
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

    #[test]
    fn a_tray_stands_when_built_and_shown() {
        for (built, smoke, host, stands) in [
            (true, false, true, true),
            (true, false, false, false),
            (true, true, false, true),
            (true, true, true, true),
            (false, false, true, false),
            (false, true, true, false),
        ] {
            assert_eq!(
                tray_stands(built, smoke, host),
                stands,
                "{built} {smoke} {host}"
            );
        }
    }

    /// With a tray, closing main hides it; without one, main is destroyed
    /// and the process ends with it.
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

    /// Only a second SIGTERM or SIGINT ends the process at once.
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

    /// The exit signals, and the codes the first two force with.
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

    /// A signal ignored at launch reads as ignored; a default or unreadable
    /// one does not. SIGUSR2, which nothing here uses, is the test's.
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

    /// A line written to a stderr nobody reads is lost without a panic.
    /// `--nocapture`, or the test harness would catch the line.
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

    /// The onboarding window's close returns while the host's reload is
    /// held on the lane, and the host then hears of it.
    #[test]
    fn the_onboarding_close_reaches_the_host_off_the_closing_thread() {
        use std::time::Duration;

        use crate::host::gated::{GatedHost, returns_at_once};

        let host = GatedHost::holding("onboarding.closed");
        let _opens = host.opens_on_drop();
        let closed = host.clone();
        assert!(
            returns_at_once(move || count_onboarding_seen(closed)),
            "the close waited for the host"
        );
        assert!(
            host.wait_held(Duration::from_secs(5)),
            "the host heard of the close"
        );
        host.open();
        assert!(host.wait_answered("onboarding.closed", Duration::from_secs(5)));
    }
}
