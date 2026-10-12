//! What the tray, the menu bar, the dock and a deep link can do without a
//! page: start or stop the recorder, open a window, switch launch at
//! login, check for updates, quit. The menus' items are `MenuAction`s,
//! and `on_menu_event` is the one handler for both menus (the tray's,
//! `tray.rs`, and on macOS the menu bar's, `menu.rs`). Recorder commands
//! are bridge methods the host answers (`recording.toggle`,
//! `recording.start`), sent through the main window as if its page had
//! sent them, so the shell holds no recorder logic of its own.
//!
//! Menu events arrive on the main thread, which runs the event loop every
//! window, the tray and the panels draw on, so nothing here calls the host
//! there: the host's call goes to Tauri's blocking pool
//! (`host::call_in_background`) and the handler returns. A Stop that waits
//! (another process holds the store's write lock, and the save tries for
//! up to 15 s) leaves the main thread free, so the recorder's own
//! `Stopping` reaches the tray at once, as its `recording` snapshot.
//!
//! Swift: `MenuBarView.swift` (the buttons), `AppCommands` in
//! `StenoApp.swift` (the Record menu).

use serde_json::{Value, json};
pub use steno_bridge::CaptureMode;
use steno_bridge::Platform;
use tauri::{
    AppHandle, Manager, WebviewWindow, Wry,
    menu::{MenuEvent, MenuId, MenuItem},
};

use crate::{
    autostart,
    bridge::{BridgeError, failed},
    host::{self, ShellHost},
    tray, updater,
    windows::{self, BridgeWindow},
};

steno_core::string_enum! {
    /// The menu items, by id (`as_str`). The ids are stable strings so a
    /// test can map them both ways without a menu.
    pub enum MenuAction {
        Record = "record",
        RecordInPerson = "record-in-person",
        OpenMain = "open-main",
        OpenSettings = "open-settings",
        LaunchAtLogin = "launch-at-login",
        CheckForUpdates = "check-for-updates",
        Quit = "quit",
    }
}

impl MenuAction {
    /// The item's text at rest on this platform ([`Self::label_on`]);
    /// `record_label` replaces the first while the recorder is busy.
    pub const fn label(self) -> &'static str {
        self.label_on(Platform::CURRENT)
    }

    /// The item's text on `platform`: the Mac's "Settings…", "Check for
    /// Updates…" and "Quit Steno", Windows' "Exit Steno", and on Windows
    /// and Linux "Settings" and "Check for Updates" without the ellipsis,
    /// which there marks an item that asks for more input before it acts;
    /// both of these act at once (the update dialog is the check's result).
    pub const fn label_on(self, platform: Platform) -> &'static str {
        match self {
            Self::Record => "Record",
            Self::RecordInPerson => "Record in person",
            Self::OpenMain => "Open Steno",
            Self::OpenSettings => platform.mac_or("Settings…", "Settings"),
            Self::LaunchAtLogin => "Launch at login",
            Self::CheckForUpdates => platform.mac_or("Check for Updates…", "Check for Updates"),
            Self::Quit => match platform {
                Platform::Windows => "Exit Steno",
                Platform::Macos | Platform::Linux => "Quit Steno",
            },
        }
    }

    /// The plain menu item for the action, in the tray's menu or the menu
    /// bar's.
    pub fn item(self, app: &AppHandle, accelerator: Option<&str>) -> tauri::Result<MenuItem<Wry>> {
        MenuItem::with_id(app, self.as_str(), self.label(), true, accelerator)
    }
}

/// The bridge method and params a tray action sends. The tray starts a
/// call through `recording.toggle` and names the mode only for an
/// in-person recording.
pub fn recorder_command(mode: Option<CaptureMode>) -> (&'static str, Value) {
    match mode {
        None => ("recording.toggle", Value::Null),
        Some(mode) => ("recording.start", json!({ "mode": mode })),
    }
}

/// The window the host answers tray commands through: the main window,
/// created when the user closed it. The host publishes the resulting
/// `recording` snapshot there, which is also where the tray and the
/// panels read it.
pub fn host_window(app: &AppHandle) -> Result<WebviewWindow, BridgeError> {
    if let Some(window) = app.get_webview_window(BridgeWindow::Main.as_str()) {
        return Ok(window);
    }
    windows::open(app, BridgeWindow::Main, None, None).map_err(failed)
}

/// Starts (`Some(mode)`) or toggles (`None`) the recorder through the host:
/// the host window is found (or made) here, on the main thread, and the
/// call runs off it ([`record_through`]).
pub fn record(app: &AppHandle, mode: Option<CaptureMode>) -> Result<(), BridgeError> {
    let window = host_window(app)?;
    record_through(app.clone(), window, mode);
    Ok(())
}

/// The host's half of [`record`]: on Tauri's blocking pool, returning at
/// once; a failure is logged from there, as a menu item has no one to
/// report it to.
pub fn record_through<H: ShellHost>(host: H, window: H::Window, mode: Option<CaptureMode>) {
    let (method, params) = recorder_command(mode);
    host::call_in_background(host, "menu", move |host| {
        host.call(&window, method, params).map(drop)
    });
}

/// Opens or focuses a window; errors are logged, as a menu item has no one
/// to report them to.
pub fn open(app: &AppHandle, window: BridgeWindow) {
    if let Err(error) = windows::open(app, window, None, None) {
        stderr_line!("[steno-desktop] opening the {window} window failed: {error}");
    }
}

/// Checks for an update and offers it, off the caller's thread; the
/// tray's item and `updates.check` from Settings both ask.
pub fn check_for_updates(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move { updater::check_and_offer(&app).await });
}

/// A menu item was chosen, in the tray's menu or the menu bar's.
pub fn on_menu_event(app: &AppHandle, event: &MenuEvent) {
    let MenuId(id) = event.id();
    let Ok(action) = id.parse::<MenuAction>() else {
        return;
    };
    match action {
        MenuAction::Record => report(record(app, None)),
        MenuAction::RecordInPerson => report(record(app, Some(CaptureMode::InPerson))),
        MenuAction::OpenMain => open(app, BridgeWindow::Main),
        MenuAction::OpenSettings => open(app, BridgeWindow::Settings),
        MenuAction::LaunchAtLogin => {
            let wanted = !autostart::status(app).is_on();
            match host_window(app) {
                Ok(window) => launch_at_login_through(app.clone(), window, wanted),
                Err(error) => {
                    report(Err(error));
                    // The check mark follows the system, even when the
                    // change failed.
                    tray::note_login_item(app);
                }
            }
        }
        MenuAction::CheckForUpdates => check_for_updates(app),
        MenuAction::Quit => quit(app),
    }
}

fn report(result: Result<(), BridgeError>) {
    if let Err(error) = result {
        stderr_line!("[steno-desktop] menu: {error}");
    }
}

/// The bridge method and params that tell the host the login item
/// changed: the same call Settings > General makes, so its snapshot
/// follows whichever menu switched it.
pub fn launch_at_login_command(value: bool) -> (&'static str, Value) {
    (
        "settings.general.setLaunchAtLogin",
        json!({ "value": value }),
    )
}

/// Switches launch at login, tells the host through `window` and moves
/// the tray's check mark, for the tray's item and for Settings alike.
/// The host hears of a failed switch too: it tries again and, failing,
/// shows why in Settings > General. Blocking (the system's login item,
/// then the host), so it runs on Tauri's blocking pool: from Settings
/// through `bridge::host_command`, from the tray through
/// [`launch_at_login_through`]. The check mark moves on the main thread,
/// once the host has answered, even when the change failed.
/// Swift: `MenuBarViewModel.setLaunchAtLogin`.
pub fn set_launch_at_login<H: ShellHost>(
    host: &H,
    window: &H::Window,
    value: bool,
) -> Result<(), BridgeError> {
    let switched = host.switch_login_item(value);
    let (method, params) = launch_at_login_command(value);
    let told = host.call(window, method, params);
    host.note_login_item();
    switched?;
    told?;
    Ok(())
}

/// The tray's Launch at login: [`set_launch_at_login`] off the main thread,
/// returning at once; a failure is logged from there.
pub fn launch_at_login_through<H: ShellHost>(host: H, window: H::Window, value: bool) {
    host::call_in_background(host, "menu", move |host| {
        set_launch_at_login(host, &window, value)
    });
}

/// The code Quit exits with.
pub const QUIT_CODE: i32 = 0;

/// Ends the process through the run loop, so `ExitRequested` carries a code
/// and `main` lets it through (a code-less request is the last window
/// closing, which the tray keeps alive) once the shutdown ran: a recording
/// in progress is saved first (`main::exit_request`). Quit in the tray's
/// menu and in the macOS menu bar, a destroyed main window with no tray,
/// and SIGTERM, SIGINT and SIGHUP all end here.
pub fn quit(app: &AppHandle) {
    app.exit(QUIT_CODE);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::host::gated::GatedHost;

    /// Runs `click` on a thread of its own, as the menu event runs on the
    /// main thread, and whether it returned within a second.
    fn returns_at_once(click: impl FnOnce() + Send + 'static) -> bool {
        let (returned, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            click();
            let _ = returned.send(());
        });
        done.recv_timeout(Duration::from_secs(1)).is_ok()
    }

    /// The tray's Record while the host's stop is held (its save waits on
    /// the store): the menu event returns at once, and the stop goes on
    /// and ends on the blocking pool.
    #[test]
    fn the_trays_record_returns_before_the_stop_ends() {
        let host = GatedHost::holding("recording.toggle");
        let _opens = host.opens_on_drop();
        let clicked = host.clone();
        assert!(
            returns_at_once(move || record_through(clicked, "main", None)),
            "the menu event waited for the stop"
        );
        assert!(
            host.wait_held(Duration::from_secs(5)),
            "the stop reached the host"
        );
        assert_eq!(host.answered(), Vec::<String>::new());
        host.open();
        assert!(host.wait_answered("recording.toggle", Duration::from_secs(5)));
    }

    /// The tray's Launch at login while the host's answer is held: the
    /// menu event returns at once, the login item is switched, and the
    /// check mark moves once the host has answered.
    #[test]
    fn the_trays_launch_at_login_returns_before_the_host_answers() {
        let (method, _) = launch_at_login_command(true);
        let host = GatedHost::holding(method);
        let _opens = host.opens_on_drop();
        let clicked = host.clone();
        assert!(
            returns_at_once(move || launch_at_login_through(clicked, "main", true)),
            "the menu event waited for the host"
        );
        assert!(
            host.wait_held(Duration::from_secs(5)),
            "the host heard of it"
        );
        assert_eq!(host.switched(), [true]);
        assert_eq!(
            host.notes(),
            0,
            "the check mark waits for the host's answer"
        );
        host.open();
        assert!(host.wait_answered(method, Duration::from_secs(5)));
        for _ in 0..100 {
            if host.notes() == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(host.notes(), 1, "the check mark follows the system");
    }

    /// The tray's Stop, on a stand-in main thread: the host publishes
    /// `Stopping` and its save then waits. The `recording` topic is
    /// throttled to 20 Hz and levels publish it all through a recording,
    /// so `Stopping` goes out from the host's flush thread, here a thread
    /// of its own. The snapshot reaches the main thread through the window
    /// sink and the tray's Record row reads "Stopping…" while the save
    /// still waits, since the menu event returned at once and left the
    /// main thread free.
    #[cfg(not(feature = "fixture-host"))]
    #[test]
    fn the_tray_reads_stopping_while_the_stop_still_runs() {
        use std::sync::{Arc, Mutex, mpsc};

        use steno_bridge::{BridgeEvent, BridgeTopic, EventSink as _};

        use crate::host::{WindowSink, real::MainThread};

        type Task = Box<dyn FnOnce() + Send>;

        /// The main thread: runs what is posted to it in order; a
        /// `recording` snapshot delivered there retitles the Record row as
        /// `tray::note_recording` does.
        #[derive(Clone)]
        struct StandInMain {
            id: std::thread::ThreadId,
            tasks: mpsc::Sender<Task>,
            record_row: Arc<Mutex<&'static str>>,
        }

        impl MainThread for StandInMain {
            fn post(&self, task: Task) {
                if std::thread::current().id() == self.id {
                    task();
                } else {
                    let _ = self.tasks.send(task);
                }
            }

            fn deliver(&self, event: BridgeEvent) {
                if let Some(state) =
                    crate::bridge::recording_state_for_shell("main", event.topic, &event.payload)
                {
                    *self.record_row.lock().unwrap() = tray::record_label(state);
                }
            }
        }

        let (tasks, posted) = mpsc::channel::<Task>();
        let thread = std::thread::spawn(move || {
            while let Ok(task) = posted.recv() {
                task();
            }
        });
        let main = StandInMain {
            id: thread.thread().id(),
            tasks,
            record_row: Arc::new(Mutex::new(tray::record_label(
                crate::recording::RecordingState::Recording,
            ))),
        };
        let sink = Arc::new(WindowSink::new(main.clone()));
        let host = GatedHost::holding("recording.toggle").before_hold(move || {
            let sink = sink.clone();
            std::thread::spawn(move || {
                sink.emit(BridgeEvent::new(
                    BridgeTopic::Recording,
                    json!({ "state": "stopping", "deniedPermissions": [] }),
                ));
            })
            .join()
            .unwrap();
        });
        let _opens = host.opens_on_drop();
        let clicked = host.clone();
        main.post(Box::new(move || record_through(clicked, "main", None)));
        assert!(
            host.wait_held(Duration::from_secs(5)),
            "the stop reached the host"
        );
        let mut row = "";
        for _ in 0..100 {
            row = *main.record_row.lock().unwrap();
            if row == "Stopping…" {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            row, "Stopping…",
            "the tray reads Stopping while the save waits"
        );
        assert_eq!(
            host.answered(),
            Vec::<String>::new(),
            "the save is still held"
        );
        host.open();
        assert!(host.wait_answered("recording.toggle", Duration::from_secs(5)));
    }

    #[test]
    fn every_action_round_trips_through_its_id() {
        for &action in MenuAction::ALL {
            assert_eq!(action.as_str().parse::<MenuAction>(), Ok(action));
            assert_eq!(action.to_string(), action.as_str());
            assert_ne!(action.label(), "");
        }
        assert!("about".parse::<MenuAction>().is_err());
    }

    #[test]
    fn each_platform_names_settings_updates_and_quit_its_own_way() {
        let labels = |platform| {
            (
                MenuAction::OpenSettings.label_on(platform),
                MenuAction::CheckForUpdates.label_on(platform),
                MenuAction::Quit.label_on(platform),
            )
        };
        assert_eq!(
            labels(Platform::Macos),
            ("Settings…", "Check for Updates…", "Quit Steno")
        );
        assert_eq!(
            labels(Platform::Windows),
            ("Settings", "Check for Updates", "Exit Steno")
        );
        assert_eq!(
            labels(Platform::Linux),
            ("Settings", "Check for Updates", "Quit Steno")
        );
        assert_eq!(
            MenuAction::Quit.label(),
            MenuAction::Quit.label_on(Platform::CURRENT)
        );
    }

    /// The tray's login item tells the host what Settings tells it, in the
    /// recorded params shape.
    #[test]
    fn the_login_item_tells_the_host_as_settings_does() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../macos/web/fixtures/bridge/params.bool.json"
        ))
        .unwrap();
        let value = fixture["value"].as_bool().unwrap();
        assert_eq!(
            launch_at_login_command(value),
            ("settings.general.setLaunchAtLogin", fixture)
        );
    }

    #[test]
    fn the_tray_sends_the_bridge_methods_the_page_sends() {
        assert_eq!(recorder_command(None), ("recording.toggle", Value::Null));
        assert_eq!(
            recorder_command(Some(CaptureMode::Call)),
            ("recording.start", json!({ "mode": "call" }))
        );
        assert_eq!(
            recorder_command(Some(CaptureMode::InPerson)),
            ("recording.start", json!({ "mode": "inPerson" }))
        );
    }

    #[test]
    fn the_mode_spells_as_the_contract_does() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../macos/web/fixtures/bridge/params.recording.start.json"
        ))
        .unwrap();
        let recorded = fixture["mode"].as_str().unwrap();
        assert!(
            CaptureMode::ALL
                .iter()
                .any(|mode| mode.as_str() == recorded),
            "{recorded}"
        );
    }
}
