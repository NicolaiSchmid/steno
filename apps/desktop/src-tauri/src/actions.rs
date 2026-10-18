//! What the tray, the menu bar, the dock and a deep link can do without a
//! page. `on_menu_event` handles both menus; recorder commands are bridge
//! methods sent through the main window, so the shell holds no recorder
//! logic. Host calls run on the shell's ordered lane
//! (`host::call_in_order`), never on the main thread, so a slow Stop
//! cannot freeze the tray and clicks keep their order.

use serde_json::{Value, json};
pub use steno_bridge::CaptureMode;
use steno_bridge::Platform;
use tauri::{
    AppHandle, Manager, WebviewWindow, Wry,
    menu::{MenuEvent, MenuId, MenuItem},
};

use crate::{
    bridge::{BridgeError, failed},
    host::{self, ShellHost},
    updater,
    windows::{self, BridgeWindow},
};

steno_core::string_enum! {
    /// The menu items, by their stable id (`as_str`).
    pub enum MenuAction {
        Record = "record",
        RecordInPerson = "record-in-person",
        OpenMain = "open-main",
        OpenSettings = "open-settings",
        LaunchAtLogin = "launch-at-login",
        CheckForUpdates = "check-for-updates",
        Quit = "quit",
        /// In the macOS menu bar's Edit menu only.
        FindMeetings = "find-meetings",
    }
}

/// The id prefix of a tray row that opens a meeting: `meeting:<uuid>`.
pub const MEETING_PREFIX: &str = "meeting:";

impl MenuAction {
    /// The item's text on this platform ([`Self::label_on`]).
    pub const fn label(self) -> &'static str {
        self.label_on(Platform::CURRENT)
    }

    /// The item's text on `platform`. The ellipsis appears on macOS only;
    /// elsewhere it marks an item that asks for more input, and these act
    /// at once.
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
            Self::FindMeetings => "Find Meetings",
        }
    }

    pub fn item(self, app: &AppHandle, accelerator: Option<&str>) -> tauri::Result<MenuItem<Wry>> {
        MenuItem::with_id(app, self.as_str(), self.label(), true, accelerator)
    }
}

/// The bridge method and params a tray action sends; the mode is named
/// only for an in-person recording.
pub fn recorder_command(mode: Option<CaptureMode>) -> (&'static str, Value) {
    match mode {
        None => ("recording.toggle", Value::Null),
        Some(mode) => ("recording.start", json!({ "mode": mode })),
    }
}

/// The window the host answers tray commands through: the main window,
/// created when the user closed it.
pub fn host_window(app: &AppHandle) -> Result<WebviewWindow, BridgeError> {
    if let Some(window) = app.get_webview_window(BridgeWindow::Main.as_str()) {
        return Ok(window);
    }
    windows::open(app, BridgeWindow::Main, None, None).map_err(failed)
}

/// Starts (`Some(mode)`) or toggles (`None`) the recorder through the host.
pub fn record(app: &AppHandle, mode: Option<CaptureMode>) -> Result<(), BridgeError> {
    let window = host_window(app)?;
    record_through(app.clone(), window, mode);
    Ok(())
}

/// The host's half of [`record`], queued on the ordered lane; a failure is
/// logged, as a menu item has no one to report it to.
pub fn record_through<H: ShellHost>(host: H, window: H::Window, mode: Option<CaptureMode>) {
    let (method, params) = recorder_command(mode);
    host::call_in_order(host, "menu", move |host| {
        host.call(&window, method, params).map(drop)
    });
}

/// Opens or focuses a window; errors are logged.
pub fn open(app: &AppHandle, window: BridgeWindow) {
    if let Err(error) = windows::open(app, window, None, None) {
        stderr_line!("[steno-desktop] opening the {window} window failed: {error}");
    }
}

/// Checks for an update and offers it, off the caller's thread.
pub fn check_for_updates(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move { updater::check_and_offer(&app).await });
}

pub fn on_menu_event(app: &AppHandle, event: &MenuEvent) {
    let MenuId(id) = event.id();
    if let Some(meeting_id) = id
        .strip_prefix(MEETING_PREFIX)
        .and_then(steno_core::json::parse_uuid)
    {
        open_meeting(app, meeting_id);
        return;
    }
    let Ok(action) = id.parse::<MenuAction>() else {
        return;
    };
    match action {
        MenuAction::Record => report(record(app, None)),
        MenuAction::RecordInPerson => report(record(app, Some(CaptureMode::InPerson))),
        MenuAction::OpenMain => open(app, BridgeWindow::Main),
        MenuAction::OpenSettings => open(app, BridgeWindow::Settings),
        MenuAction::LaunchAtLogin => match host_window(app) {
            Ok(window) => launch_at_login_through(app.clone(), window),
            Err(error) => {
                report(Err(error));
                // The check mark follows the system, even after a failure.
                host::call_in_order(app.clone(), "menu", |host| {
                    host.note_login_item(host.login_item_on());
                    Ok(())
                });
            }
        },
        MenuAction::CheckForUpdates => check_for_updates(app),
        MenuAction::Quit => quit_from_menu(app),
        MenuAction::FindMeetings => find_meetings(app),
    }
}

fn open_meeting(app: &AppHandle, meeting_id: uuid::Uuid) {
    let request = steno_bridge::WindowParams {
        window: BridgeWindow::Main,
        section: None,
        meeting_id: Some(meeting_id),
    };
    if let Err(error) = windows::open_requested(app, &request) {
        stderr_line!("[steno-desktop] opening a meeting failed: {error}");
    }
}

/// The page's own ⌘F, dispatched in the main window to focus its search.
const FIND_MEETINGS_SCRIPT: &str = "window.dispatchEvent(new KeyboardEvent('keydown', \
    { key: 'f', code: 'KeyF', metaKey: true }));";

fn find_meetings(app: &AppHandle) {
    let shown = windows::open(app, BridgeWindow::Main, None, None)
        .and_then(|window| window.eval(FIND_MEETINGS_SCRIPT));
    if let Err(error) = shown {
        stderr_line!("[steno-desktop] Find Meetings failed: {error}");
    }
}

fn report(result: Result<(), BridgeError>) {
    if let Err(error) = result {
        stderr_line!("[steno-desktop] menu: {error}");
    }
}

/// The call Settings > General makes when the login item changed, so the
/// host's snapshot follows whichever surface switched it.
pub fn launch_at_login_command(value: bool) -> (&'static str, Value) {
    (
        "settings.general.setLaunchAtLogin",
        json!({ "value": value }),
    )
}

/// Switches launch at login, tells the host through `window` (also on
/// failure, so Settings can show why) and moves the tray's check mark to
/// what the system then says. Blocking, so it runs off the main thread.
pub fn set_launch_at_login<H: ShellHost>(
    host: &H,
    window: &H::Window,
    value: bool,
) -> Result<(), BridgeError> {
    let switched = host.switch_login_item(value);
    let (method, params) = launch_at_login_command(value);
    let told = host.call(window, method, params);
    host.note_login_item(host.login_item_on());
    switched?;
    told?;
    Ok(())
}

/// The tray's Launch at login on the ordered lane. It reads the system's
/// state once the clicks before it have landed, so a second click undoes
/// the first.
pub fn launch_at_login_through<H: ShellHost>(host: H, window: H::Window) {
    host::call_in_order(host, "menu", move |host| {
        let wanted = !host.login_item_on();
        set_launch_at_login(host, &window, wanted)
    });
}

pub const QUIT_CODE: i32 = 0;

/// Ends the process through the run loop with an exit code, so `main`
/// lets the request through once the shutdown has saved any recording (a
/// code-less request is the last window closing, which the tray keeps
/// alive).
pub fn quit(app: &AppHandle) {
    app.exit(QUIT_CODE);
}

/// The menus' Quit ([`quit_through`] with
/// [`SHUTDOWN_PATIENCE`](steno_services::app::SHUTDOWN_PATIENCE)).
pub fn quit_from_menu(app: &AppHandle) {
    let exiting = app.clone();
    quit_through(app, steno_services::app::SHUTDOWN_PATIENCE, move || {
        quit(&exiting);
    });
}

/// The menus' Quit on `host`, on the main thread. Refuses every recording
/// start at once and marks the tray as quitting, then exits once the
/// actions clicked before it have run ([`exit_after_queued`]) or
/// `patience` has passed; a save still waiting then is left to the next
/// launch's recovery.
pub fn quit_through<H: ShellHost>(
    host: &H,
    patience: std::time::Duration,
    exit: impl FnOnce() + Send + 'static,
) {
    host.refuse_starts();
    host.note_quitting();
    exit_after_queued(host, patience, exit);
}

/// Calls `exit` from a thread of its own once every job queued on `host`'s
/// lane before this call has run, or after `patience`; returns at once.
pub fn exit_after_queued<H: ShellHost>(
    host: &H,
    patience: std::time::Duration,
    exit: impl FnOnce() + Send + 'static,
) {
    let (reached, queued_before) = std::sync::mpsc::channel::<()>();
    host.queue(
        "quit",
        Box::new(move || {
            let _ = reached.send(());
        }),
    );
    std::thread::spawn(move || {
        let _ = queued_before.recv_timeout(patience);
        exit();
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::host::gated::{GatedHost, Opens, returns_at_once};

    /// A host whose stop is held until the returned guard drops.
    fn a_held_stop() -> (GatedHost, Opens) {
        let host = GatedHost::holding("recording.toggle");
        let opens = host.opens_on_drop();
        record_through(host.clone(), "main", None);
        assert!(
            host.wait_held(Duration::from_secs(5)),
            "the stop reached the host"
        );
        (host, opens)
    }

    /// The tray's Record during a held stop returns at once.
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

    /// The tray's Launch at login returns at once while the host is held,
    /// and the check mark moves once the host has answered.
    #[test]
    fn the_trays_launch_at_login_returns_before_the_host_answers() {
        let (method, _) = launch_at_login_command(true);
        let host = GatedHost::holding(method);
        let _opens = host.opens_on_drop();
        let clicked = host.clone();
        assert!(
            returns_at_once(move || launch_at_login_through(clicked, "main")),
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
        assert!(
            host.wait_noted(1, Duration::from_secs(1)),
            "the check mark follows the system"
        );
        assert_eq!(host.checked(), [true], "as read once switched");
    }

    /// The tray's Record row reads "Stopping…" while the stop's save still
    /// waits, since the menu event left the main thread free.
    #[cfg(not(feature = "fixture-host"))]
    #[test]
    fn the_tray_reads_stopping_while_the_stop_still_runs() {
        use std::sync::{Arc, mpsc};

        use steno_bridge::{BridgeEvent, BridgeTopic, EventSink as _};

        use crate::host::{WindowSink, real::MainThread};

        type Task = Box<dyn FnOnce() + Send>;

        /// A stand-in main thread: runs posted tasks in order and records
        /// the Record row's text as `tray::note_recording` sets it.
        #[derive(Clone)]
        struct StandInMain {
            id: std::thread::ThreadId,
            tasks: mpsc::Sender<Task>,
            rows: mpsc::Sender<&'static str>,
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
                    let _ = self.rows.send(crate::tray::record_label(state));
                }
            }
        }

        let (tasks, posted) = mpsc::channel::<Task>();
        let thread = std::thread::spawn(move || {
            while let Ok(task) = posted.recv() {
                task();
            }
        });
        let (rows, retitled) = mpsc::channel();
        let main = StandInMain {
            id: thread.thread().id(),
            tasks,
            rows,
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
        assert_eq!(
            retitled.recv_timeout(Duration::from_secs(1)),
            Ok("Stopping…"),
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

    /// Two quick tray clicks reach the host in the order they were made.
    #[test]
    fn two_quick_tray_clicks_reach_the_host_in_click_order() {
        let rounds = 300;
        let reordered = (0..rounds)
            .filter(|_| {
                let host = GatedHost::holding("nothing is held");
                record_through(host.clone(), "main", Some(CaptureMode::InPerson));
                record_through(host.clone(), "main", None);
                assert!(host.wait_answers(2, Duration::from_secs(5)));
                host.answered() != ["recording.start", "recording.toggle"]
            })
            .count();
        assert_eq!(
            reordered, 0,
            "{reordered} of {rounds} click pairs were reordered"
        );
    }

    /// A Record clicked during a waiting Stop runs after it and starts a
    /// recording rather than being dropped.
    #[test]
    fn a_record_clicked_during_a_held_stop_runs_after_it() {
        let (host, _opens) = a_held_stop();
        record_through(host.clone(), "main", Some(CaptureMode::InPerson));
        assert!(
            !host.wait_answered("recording.start", Duration::from_millis(500)),
            "the Record ran beside the held stop"
        );
        host.open();
        assert!(host.wait_answered("recording.start", Duration::from_secs(5)));
        assert_eq!(host.answered(), ["recording.toggle", "recording.start"]);
    }

    /// Two quick Launch at login clicks switch the item on and then off.
    #[test]
    fn a_second_launch_at_login_click_undoes_the_first() {
        let (host, _opens) = a_held_stop();
        launch_at_login_through(host.clone(), "main");
        launch_at_login_through(host.clone(), "main");
        host.open();
        assert!(host.wait_noted(2, Duration::from_secs(5)));
        assert_eq!(host.switched(), [true, false]);
        assert_eq!(host.checked(), [true, false]);
    }

    /// A refused switch still reaches the host, and the check mark shows
    /// what the system says.
    #[test]
    fn a_refused_launch_at_login_leaves_the_check_mark_on_the_system() {
        let (method, _) = launch_at_login_command(true);
        let host = GatedHost::holding("nothing is held").failing_switch();
        launch_at_login_through(host.clone(), "main");
        assert!(host.wait_noted(1, Duration::from_secs(5)));
        assert_eq!(host.switched(), [true]);
        assert_eq!(host.answered(), [method]);
        assert_eq!(host.checked(), [false]);
    }

    /// Quit waits for the Stop clicked before it, then exits.
    #[test]
    fn a_quit_waits_for_the_stop_clicked_before_it() {
        let (host, _opens) = a_held_stop();
        let (exited, exit) = std::sync::mpsc::channel();
        let seen = host.clone();
        exit_after_queued(&host, Duration::from_secs(5), move || {
            let _ = exited.send(seen.answered());
        });
        assert!(
            exit.recv_timeout(Duration::from_millis(500)).is_err(),
            "the exit came while the stop was held"
        );
        host.open();
        assert_eq!(
            exit.recv_timeout(Duration::from_secs(5)),
            Ok(vec!["recording.toggle".to_owned()]),
            "the exit comes once the stop has answered"
        );
    }

    /// A stop still waiting after the patience does not hold the exit.
    #[test]
    fn a_quit_goes_ahead_once_its_patience_has_passed() {
        let (host, _opens) = a_held_stop();
        let (exited, exit) = std::sync::mpsc::channel();
        let asked = std::time::Instant::now();
        exit_after_queued(&host, Duration::from_millis(200), move || {
            let _ = exited.send(asked.elapsed());
        });
        let after = exit.recv_timeout(Duration::from_secs(10)).expect("an exit");
        assert!(after >= Duration::from_millis(190), "{after:?}");
        assert!(after < Duration::from_millis(1500), "{after:?}");
        assert_eq!(
            host.answered(),
            Vec::<String>::new(),
            "the stop is still held"
        );
    }

    /// Quit refuses starts and retitles the tray before returning, without
    /// waiting behind a pending stop.
    #[test]
    fn a_quit_refuses_starts_and_retitles_the_tray_at_once() {
        let (host, _opens) = a_held_stop();
        let clicked = host.clone();
        assert!(
            returns_at_once(move || quit_through(&clicked, Duration::from_secs(5), || {})),
            "the Quit waited"
        );
        assert_eq!(host.quit_steps(), ["refuse starts", "quitting"]);
        assert_eq!(host.answered(), Vec::<String>::new(), "the stop is held");
    }

    /// A Record queued between a waiting Stop and Quit is refused, so no
    /// recording is left to save on the way out.
    #[test]
    fn a_record_queued_before_a_quit_is_refused_before_the_exit() {
        let host = GatedHost::holding("recording.toggle")
            .delaying("recording.start", Duration::from_millis(300));
        let _opens = host.opens_on_drop();
        record_through(host.clone(), "main", None);
        assert!(
            host.wait_held(Duration::from_secs(5)),
            "the stop reached the host"
        );
        record_through(host.clone(), "main", Some(CaptureMode::InPerson));
        let (exited, exit) = std::sync::mpsc::channel();
        let seen = host.clone();
        quit_through(&host, Duration::from_secs(5), move || {
            let _ = exited.send(seen.answered());
        });
        assert!(
            exit.recv_timeout(Duration::from_millis(300)).is_err(),
            "the exit came while the stop was held"
        );
        host.open();
        assert_eq!(
            exit.recv_timeout(Duration::from_secs(5)),
            Ok(vec![
                "recording.toggle".to_owned(),
                "recording.start refused".to_owned()
            ]),
            "the Record queued before the Quit ran, refused, before the exit"
        );
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

    /// The tray's login item sends the same params Settings does.
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
