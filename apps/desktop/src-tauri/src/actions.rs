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
//! there: the host's call is queued on the shell's ordered lane
//! (`host::call_in_order`) and the handler returns. A Stop that waits
//! (another process holds the store's write lock, and the save tries for
//! up to 15 s) leaves the main thread free, so the recorder's own
//! `Stopping` reaches the tray at once, as its `recording` snapshot. The
//! lane keeps the clicks in order, as the main thread did: a Record
//! clicked during that stop runs once it has ended, and Quit waits for it
//! ([`quit_from_menu`]), having refused every start first.
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
    bridge::{BridgeError, failed},
    host::{self, ShellHost},
    updater,
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
        /// In the macOS menu bar's Edit menu only.
        FindMeetings = "find-meetings",
    }
}

/// The id prefix of a tray row that opens a meeting: `meeting:<uuid>`.
pub const MEETING_PREFIX: &str = "meeting:";

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
            Self::FindMeetings => "Find Meetings",
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
/// call is queued off it ([`record_through`]).
pub fn record(app: &AppHandle, mode: Option<CaptureMode>) -> Result<(), BridgeError> {
    let window = host_window(app)?;
    record_through(app.clone(), window, mode);
    Ok(())
}

/// The host's half of [`record`]: on the shell's ordered lane, after the
/// clicks before it, returning at once; a failure is logged from there, as
/// a menu item has no one to report it to.
pub fn record_through<H: ShellHost>(host: H, window: H::Window, mode: Option<CaptureMode>) {
    let (method, params) = recorder_command(mode);
    host::call_in_order(host, "menu", move |host| {
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
                // The check mark follows the system, even when the change
                // failed, read off the main thread.
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

/// Opens the main window on `meeting_id`, as a deep link does.
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

/// The page's own ⌘F, pressed in the main window: its search field takes
/// the focus (`useShortcut` in `meeting-list.tsx`).
const FIND_MEETINGS_SCRIPT: &str = "window.dispatchEvent(new KeyboardEvent('keydown', \
    { key: 'f', code: 'KeyF', metaKey: true }));";

/// Find Meetings: brings the main window up and focuses its search.
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
/// then the host), so it runs off the main thread: from Settings on
/// Tauri's blocking pool (`bridge::host_command`), from the tray on the
/// shell's ordered lane ([`launch_at_login_through`]). Once the host has
/// answered, even when the change failed, the system's login item is read
/// again here and the check mark shows it, on the main thread.
/// Swift: `MenuBarViewModel.setLaunchAtLogin`.
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

/// The tray's Launch at login: [`set_launch_at_login`] on the shell's
/// ordered lane, returning at once; a failure is logged from there. The
/// item switches what the system says once the clicks before it have
/// landed, read there, so a second click undoes the first.
pub fn launch_at_login_through<H: ShellHost>(host: H, window: H::Window) {
    host::call_in_order(host, "menu", move |host| {
        let wanted = !host.login_item_on();
        set_launch_at_login(host, &window, wanted)
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

/// The menus' Quit ([`quit_through`] with
/// [`SHUTDOWN_PATIENCE`](steno_services::app::SHUTDOWN_PATIENCE)).
pub fn quit_from_menu(app: &AppHandle) {
    let exiting = app.clone();
    quit_through(app, steno_services::app::SHUTDOWN_PATIENCE, move || {
        quit(&exiting);
    });
}

/// The menus' Quit on `host`, on the main thread. Every recording start is
/// refused at once (`ShellHost::refuse_starts`), so a Record from any
/// surface, the tray's queued before the Quit included, says "Steno is
/// quitting" and leaves no meeting to save on the way out; the tray's
/// Record rows read "Quitting…", disabled, and Launch at login is
/// disabled (`tray::note_quitting`). Then `exit` waits for the actions
/// clicked before the Quit ([`exit_after_queued`]): a Stop whose save
/// waits on the store finishes it, with its own end reason, as it did when
/// the main thread ran the Stop and the Quit waited behind it. Past
/// `patience` the exit goes ahead: its shutdown tries the stop's commit no
/// more, and a save still waiting leaves the meeting `recording` for the
/// next launch's recovery.
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
/// lane before this call has run, or once `patience` has passed, whichever
/// comes first; returns at once.
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

    /// A host whose stop is held (its save waits on the store), with the
    /// tray's Stop clicked and waiting at the gate, which opens when the
    /// guard drops.
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

    /// The tray's Record while the host's stop is held (its save waits on
    /// the store): the menu event returns at once, and the stop goes on
    /// and ends on the lane.
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

    /// The tray's Stop, on a stand-in main thread: the host publishes
    /// `Stopping` and its save then waits. The `recording` topic is
    /// throttled to 20 Hz and levels publish it all through a recording,
    /// so `Stopping` goes out from the host's `recording` feed, a thread
    /// of its own. The snapshot reaches the main thread through the window
    /// sink and the tray's Record row reads "Stopping…" while the save
    /// still waits, since the menu event returned at once and left the
    /// main thread free.
    #[cfg(not(feature = "fixture-host"))]
    #[test]
    fn the_tray_reads_stopping_while_the_stop_still_runs() {
        use std::sync::{Arc, mpsc};

        use steno_bridge::{BridgeEvent, BridgeTopic, EventSink as _};

        use crate::host::{WindowSink, real::MainThread};

        type Task = Box<dyn FnOnce() + Send>;

        /// The main thread: runs what is posted to it in order; a
        /// `recording` snapshot delivered there retitles the Record row as
        /// `tray::note_recording` does, and the row's text goes to `rows`.
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

    /// Two quick tray clicks, Record in person and then Record (a
    /// toggle), reach the host in the order they were made, as they did
    /// when the main thread ran them one after the other. Reordered, the
    /// toggle would start a call recording and the in-person start be
    /// refused; spawned one per click on Tauri's pools, some pairs are.
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

    /// A Record clicked while the Stop before it waits on the store (the
    /// recorder reads `Stopping`, where a start is ignored) runs once that
    /// stop has ended, so it starts a recording rather than being dropped.
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

    /// Two quick Launch at login clicks, made while the Stop clicked before
    /// them still waits on the store, switch the item on and then off: the
    /// second reads what the system says once the first has landed, where
    /// reading it at the click would repeat the first.
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

    /// A Launch at login whose switch the system refuses: the host still
    /// hears of it, and the check mark shows what the system says, read
    /// in the job once the host answered, not what was asked.
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

    /// The menus' Quit while the Stop clicked before it waits on the
    /// store: the exit waits for that stop, so its save finishes with its
    /// own end reason, as when the main thread ran the Stop and the Quit
    /// waited behind it; then the exit comes.
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

    /// A stop that still waits once the patience has passed does not hold
    /// the exit: it goes ahead about when the patience ends, never seconds
    /// later, and the shutdown takes over the stop.
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

    /// The menus' Quit refuses every start and retitles the tray before it
    /// returns, on the main thread, while the Stop clicked before it still
    /// waits on the store: neither waits behind that stop on the lane.
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

    /// A Record clicked while the Stop before it waits on the store, then
    /// Quit: the Record runs after the stop and before the exit, and is
    /// refused, as the Quit refused every start before either ran, so no
    /// recording is left to save on the way out. Its call takes 300 ms
    /// here, so an exit that came before it would find it unanswered.
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
