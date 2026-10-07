//! Linux: a logout and a system shutdown or reboot, which reach the app as
//! no exit request of their own. Three D-Bus clients, each on a thread of
//! its own, run the shutdown Quit runs before the app goes (`SaveAndQuit`)
//! or tell the desktop a recording runs:
//!
//! - **A logout** (`follow_session_end`), through the session manager on
//!   GNOME and on Xfce (`follow_session`): the app registers as a client
//!   on the session bus (`RegisterClient` on GNOME's
//!   `org.gnome.SessionManager`, else Xfce's `org.xfce.SessionManager`,
//!   `SessionApi`). It finds the manager's unique name with
//!   `GetNameOwner`, so it starts none, and takes the client signals from
//!   that name only. On GNOME it answers `QueryEndSession` at once
//!   (gnome-session asks before its confirmation dialog, which the user
//!   can still cancel, and gives a query one second), and on `EndSession`
//!   saves, answers `EndSessionResponse` and quits; gnome-session waits
//!   about ten seconds for that answer. On Xfce it saves at
//!   `QueryEndSession`, answers and quits: xfce4-session asks once the
//!   user chose to log out and waits up to a minute for the answer, but
//!   only seven seconds after `EndSession`, and on Wayland it quits after
//!   the query without sending `EndSession`. Where no session manager runs
//!   (KDE Plasma, wlroots desktops), through the desktop portal's session
//!   monitor (`follow_portal`): the app opens it (`CreateMonitor` on
//!   `org.freedesktop.portal.Inhibit`), answers query-end at once
//!   (`QueryEndResponse`; the portal gives a second, and the end can still
//!   be called off), and at ending saves and quits. Plasma 6.6's portal
//!   serves the monitor, but nothing in Plasma 6.6 asks it yet, so it
//!   never reports the end there; Plasma before 6.6 and the GTK portal off
//!   GNOME report no end.
//! - **The logout inhibitor** (`hold_logout_inhibitor`, told through
//!   `LogoutInhibitor`): while a recording runs the app holds the portal's
//!   `Inhibit` with the `Logout` flag. GNOME shows it in its logout
//!   dialog, Plasma 6.6 notes it for its monitor, and the GTK portal off
//!   GNOME refuses it.
//! - **A system shutdown or reboot** (`hold_shutdown_lock`): the app holds
//!   logind's `shutdown` delay lock (`Inhibit` on the system bus), and on
//!   `PrepareForShutdown(true)` it saves and then releases the lock. logind
//!   waits for the lock at most its `InhibitDelayMaxSec`, five seconds by
//!   default, and then goes ahead; the SIGTERM that follows waits for the
//!   save in progress (`exit_on_signals` in `main.rs`).
//!
//! Every desktop then ends the display server, and GDK would end the
//! process with it; the log writer in `display_lost` holds that exit until
//! the save in progress, or one it starts, has ended. So a save that
//! outlasts a session manager's or logind's wait still ends, at most
//! `SHUTDOWN_PATIENCE` after it began, unless something kills the process
//! first (systemd's `SIGKILL` once a stop has waited its timeout, 90 s by
//! default). On KDE Plasma, which does not ask the app, that is the save.
//!
//! None of it follows sleep or the screen lock: a recording goes on
//! through both, as it does on the Mac. A bus that is missing or refuses, a
//! session manager or portal that is not running, or a lock logind denies
//! leaves the app to the signals and the lost display. A slow or frozen
//! bus holds only its client's thread, never the launch or an exit.
//!
//! Swift: none; `AppKit` sends a logout and a shutdown to
//! `applicationShouldTerminate`, which Quit goes through too.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use tauri::Manager as _;
use zbus::blocking::connection::Builder;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type;
use zbus::names::{BusName, OwnedUniqueName};
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedFd, OwnedObjectPath, OwnedValue, Value};

/// A session manager's client protocol: GNOME's, which Xfce serves under
/// names of its own (GTK 3.24 falls back to them too).
#[derive(Debug, Clone, Copy)]
struct SessionApi {
    /// The manager's name on the session bus.
    name: &'static str,
    /// Its object.
    path: &'static str,
    /// The interface a client registers through (`RegisterClient`).
    manager: &'static str,
    /// The interface of a registered client's object: the manager's
    /// signals to it and its answer (`EndSessionResponse`).
    client: &'static str,
    /// Whether the client saves at `QueryEndSession` rather than at
    /// `EndSession` (`client_step`).
    saves_at_query: bool,
}

impl SessionApi {
    const GNOME: Self = Self {
        name: "org.gnome.SessionManager",
        path: "/org/gnome/SessionManager",
        manager: "org.gnome.SessionManager",
        client: "org.gnome.SessionManager.ClientPrivate",
        // gnome-session asks before its confirmation dialog, which the user
        // can still cancel, and gives a query one second.
        saves_at_query: false,
    };
    const XFCE: Self = Self {
        name: "org.xfce.SessionManager",
        path: "/org/xfce/SessionManager",
        manager: "org.xfce.Session.Manager",
        client: "org.xfce.Session.Client",
        // xfce4-session asks once the user chose to log out, waits up to a
        // minute for the answer and seven seconds after `EndSession`, and
        // on Wayland quits after the query without sending `EndSession`.
        saves_at_query: true,
    };
    /// In the order the app looks for them on the bus.
    const ALL: [Self; 2] = [Self::GNOME, Self::XFCE];
}

/// The id the session manager knows the app by: its desktop entry's name
/// (`linux/steno-desktop.desktop`) without the suffix.
const APP_ID: &str = "steno-desktop";

/// logind, its manager object and interface.
const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";

/// How long a client waits for a method's answer: a frozen bus then holds
/// it this long at most, so `EndSession` still quits.
const CALL_PATIENCE: Duration = Duration::from_secs(5);

/// What the app does when its session or the system ends: `save` runs the
/// shutdown Quit runs and returns once it ended, at most
/// `SHUTDOWN_PATIENCE` later (at once when it already ran); `quit` asks
/// for the exit, which then goes through at once.
#[derive(Clone)]
pub struct SaveAndQuit {
    save: Arc<dyn Fn() + Send + Sync>,
    quit: Arc<dyn Fn() + Send + Sync>,
}

impl SaveAndQuit {
    /// The app's: the save before an end (`save_before_end` in `main.rs`),
    /// then Quit.
    fn of(app: &tauri::AppHandle) -> Self {
        let (saving, quitting) = (app.clone(), app.clone());
        Self {
            save: Arc::new(move || crate::save_before_end(&saving)),
            quit: Arc::new(move || crate::actions::quit(&quitting)),
        }
    }
}

/// The startup id the session manager gave the app when it started it (an
/// autostart at login), so it knows the client; empty otherwise. Read
/// before Tauri builds the app: GTK unsets `DESKTOP_AUTOSTART_ID` when it
/// starts.
pub fn startup_id() -> String {
    std::env::var("DESKTOP_AUTOSTART_ID").unwrap_or_default()
}

/// Starts both clients, the session's only with a session bus (the
/// definition the single instance uses, `session_bus_named`); `startup_id`
/// is what `startup_id` read at launch.
pub fn watch(app: &tauri::AppHandle, startup_id: String) {
    let on_end = SaveAndQuit::of(app);
    if crate::session_bus_named() {
        let on_end = on_end.clone();
        spawn_client(
            "steno-session-client",
            "a logout saves only when a signal reaches the app or the display closes",
            move || follow_session_end(&patient(Builder::session()?)?, &startup_id, &on_end),
        );
        let (busy, recording) = mpsc::channel();
        app.manage(LogoutInhibitor::new(busy));
        spawn_client(
            "steno-logout-inhibitor",
            "a recording does not hold a logout back",
            move || {
                hold_logout_inhibitor(&patient(Builder::session()?)?, &recording);
                Ok(())
            },
        );
    }
    spawn_client(
        "steno-shutdown-lock",
        "a system shutdown or reboot saves only when a signal reaches the app",
        move || hold_shutdown_lock(&patient(Builder::system()?)?, &on_end),
    );
}

/// The connection `builder` makes, whose method calls wait `CALL_PATIENCE`
/// at most.
fn patient(builder: Builder<'_>) -> zbus::Result<Connection> {
    builder.method_timeout(CALL_PATIENCE).build()
}

/// Runs `client` on a thread named `name`; an error ends it with `gap`,
/// what the app is left without, in the log.
fn spawn_client(
    name: &'static str,
    gap: &'static str,
    client: impl FnOnce() -> zbus::Result<()> + Send + 'static,
) {
    let spawned = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            if let Err(error) = client() {
                tracing::info!(%error, client = name, "{gap}");
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, client = name, "{gap}");
    }
}

/// A proxy that caches no property: neither client reads one, and the
/// cache would ask the bus for them.
fn proxy<'a>(
    connection: &Connection,
    destination: &'a str,
    path: &'a str,
    interface: &'a str,
) -> zbus::Result<Proxy<'a>> {
    zbus::blocking::proxy::Builder::new(connection)
        .destination(destination)?
        .path(path)?
        .interface(interface)?
        .cache_properties(CacheProperties::No)
        .build()
}

/// What a session client does on one of the session manager's signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientStep {
    /// Answers yes at once: a recording does not hold a logout back.
    Answer,
    /// Saves, answers, then quits: the session ends.
    SaveAnswerQuit,
    /// Quits: the session manager asks the client to leave.
    Quit,
    /// Nothing (`CancelEndSession`, and a signal this client does not know).
    Wait,
}

/// The step for the session manager's signal `member` under `api`: the
/// query is answered at once, or saved for first where the manager asks
/// only once the session is ending (`SessionApi::saves_at_query`).
fn client_step(api: SessionApi, member: &str) -> ClientStep {
    match member {
        "QueryEndSession" if api.saves_at_query => ClientStep::SaveAnswerQuit,
        "QueryEndSession" => ClientStep::Answer,
        "EndSession" => ClientStep::SaveAnswerQuit,
        "Stop" => ClientStep::Quit,
        _ => ClientStep::Wait,
    }
}

/// The bus's own proxy, which caches no property.
fn bus(connection: &Connection) -> zbus::Result<zbus::blocking::fdo::DBusProxy<'_>> {
    zbus::blocking::fdo::DBusProxy::builder(connection)
        .cache_properties(CacheProperties::No)
        .build()
}

/// The unique name that owns `name` on `connection`, none when no peer
/// does. Asked of the bus (`GetNameOwner`), so asking starts none.
fn owner_of(connection: &Connection, name: &str) -> zbus::Result<Option<OwnedUniqueName>> {
    match bus(connection)?.get_name_owner(BusName::try_from(name)?) {
        Ok(owner) => Ok(Some(owner)),
        Err(zbus::fdo::Error::NameHasNoOwner(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The first session manager of `SessionApi::ALL` on `session`, with the
/// unique name that owns it; none when neither runs.
fn session_manager(session: &Connection) -> zbus::Result<Option<(SessionApi, OwnedUniqueName)>> {
    for api in SessionApi::ALL {
        if let Some(owner) = owner_of(session, api.name)? {
            return Ok(Some((api, owner)));
        }
    }
    Ok(None)
}

/// Follows the end of the session on `session`: as a client of its
/// session manager when one runs (`follow_session`), else through the
/// desktop portal's session monitor (`follow_portal`).
fn follow_session_end(
    session: &Connection,
    startup_id: &str,
    on_end: &SaveAndQuit,
) -> zbus::Result<()> {
    match session_manager(session)? {
        Some(manager) => follow_session(session, manager, startup_id, on_end),
        None => follow_portal(session, on_end),
    }
}

/// Registers the app with `manager`, the session manager on `session`,
/// and follows its signals to this client until the session ends; an
/// error when the bus goes away before the app quit.
fn follow_session(
    session: &Connection,
    (api, owner): (SessionApi, OwnedUniqueName),
    startup_id: &str,
    on_end: &SaveAndQuit,
) -> zbus::Result<()> {
    // Subscribed before the app registers, so no signal falls between. The
    // rule names the manager's unique name, which zbus also checks on
    // every message it delivers, so a signal another peer sends straight
    // to the app, past the bus's rules, is dropped too.
    let rule = zbus::MatchRule::builder()
        .msg_type(Type::Signal)
        .sender(owner.as_str())?
        .interface(api.client)?
        .build();
    let signals = MessageIterator::for_match_rule(rule, session, None)?;
    let manager = proxy(session, owner.as_str(), api.path, api.manager)?;
    let client: OwnedObjectPath = manager.call("RegisterClient", &(APP_ID, startup_id))?;
    let answer = || {
        session
            .call_method(
                Some(owner.as_str()),
                client.as_str(),
                Some(api.client),
                "EndSessionResponse",
                &(true, ""),
            )
            .map(drop)
    };
    for signal in signals {
        let signal = signal?;
        let header = signal.header();
        // Another client's signal.
        if header.path().map(zbus::zvariant::ObjectPath::as_str) != Some(client.as_str()) {
            continue;
        }
        let Some(member) = header.member() else {
            continue;
        };
        match client_step(api, member.as_str()) {
            // A lost answer leaves the client following: the end may still
            // come.
            ClientStep::Answer => {
                if let Err(error) = answer() {
                    tracing::warn!(%error, "the session manager's query went unanswered");
                }
            }
            ClientStep::SaveAnswerQuit => {
                tracing::info!(signal = member.as_str(), "the session is ending; saving");
                (on_end.save)();
                let answered = answer();
                (on_end.quit)();
                return answered;
            }
            ClientStep::Quit => {
                (on_end.quit)();
                return Ok(());
            }
            ClientStep::Wait => {}
        }
    }
    Err(zbus::Error::Failure("the session bus closed".to_owned()))
}

/// Holds logind's `shutdown` delay lock on `system` until a system
/// shutdown or reboot begins, then saves, releases it and quits; an error
/// when logind is not there, denies the lock, or the bus goes away before
/// the app quit.
fn hold_shutdown_lock(system: &Connection, on_end: &SaveAndQuit) -> zbus::Result<()> {
    let manager = proxy(system, LOGIND, LOGIND_PATH, LOGIND_MANAGER)?;
    // Subscribed before the lock is taken, so no shutdown falls between.
    let shutdowns = manager.receive_signal("PrepareForShutdown")?;
    let lock: OwnedFd = manager.call(
        "Inhibit",
        &(
            "shutdown",
            "Steno",
            "Saves the recording in progress",
            "delay",
        ),
    )?;
    for signal in shutdowns {
        // `false` reports a shutdown called off, which no lock needs.
        if signal.body().deserialize::<bool>()? {
            tracing::info!("the system is shutting down; saving");
            (on_end.save)();
            drop(lock);
            (on_end.quit)();
            return Ok(());
        }
    }
    Err(zbus::Error::Failure("the system bus closed".to_owned()))
}

/// The desktop portal, its object, and the interfaces of its inhibitor
/// and of the requests it hands out.
const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_INHIBIT: &str = "org.freedesktop.portal.Inhibit";
const PORTAL_REQUEST: &str = "org.freedesktop.portal.Request";

/// A portal's dictionary of named values (`a{sv}`): the results of a
/// request's `Response`, a monitor's state, the options of a call.
type Options = HashMap<String, OwnedValue>;

/// The token of the app's session monitor, which names its request and
/// its session (`portal_paths`).
const MONITOR_TOKEN: &str = "steno_session_monitor";

/// The `Logout` flag of the portal's `Inhibit`.
const INHIBIT_LOGOUT: u32 = 1;

/// The reason a logout inhibitor gives, which the desktop may show in its
/// logout dialog.
const INHIBIT_REASON: &str = "A meeting is being recorded";

/// The objects the portal names after `sender`, the client's unique name,
/// and `token`: the request of a call and the session it creates
/// (`org.freedesktop.portal.Request`, `.Session`).
fn portal_paths(sender: &str, token: &str) -> (String, String) {
    let sender = sender.trim_start_matches(':').replace('.', "_");
    (
        format!("{PORTAL_PATH}/request/{sender}/{token}"),
        format!("{PORTAL_PATH}/session/{sender}/{token}"),
    )
}

/// What the monitor client does on a session state the portal reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MonitorStep {
    /// The session is being asked to end (2, query-end): answers at once
    /// (`QueryEndResponse`) and goes on following. The portal gives the
    /// answer a second, and the end can still be called off (the session
    /// goes back to running): a desktop that asks may wait on the app's
    /// own logout inhibitor, so the recording runs on until the end.
    Answer,
    /// The session is ending (3): saves, then quits.
    SaveQuit,
    /// Running (1), unknown, or none.
    Wait,
}

/// The step for the portal's `session-state`.
fn monitor_step(state: Option<u32>) -> MonitorStep {
    match state {
        Some(2) => MonitorStep::Answer,
        Some(3) => MonitorStep::SaveQuit,
        _ => MonitorStep::Wait,
    }
}

/// Opens the desktop portal's session monitor (`CreateMonitor` on
/// `org.freedesktop.portal.Inhibit`) and follows its `StateChanged` until
/// the session ends: it answers a query at once (`QueryEndResponse`) and
/// at the end saves and quits (`monitor_step`). Started when needed (the
/// portal is activated on demand); its signals are taken from its unique
/// name only. An error when the portal is missing, refuses the monitor, or
/// the bus goes away before the app quit.
fn follow_portal(session: &Connection, on_end: &SaveAndQuit) -> zbus::Result<()> {
    let owner = if let Some(owner) = owner_of(session, PORTAL)? {
        owner
    } else {
        bus(session)?.start_service_by_name(PORTAL.try_into()?, 0)?;
        owner_of(session, PORTAL)?
            .ok_or_else(|| zbus::Error::Failure("the portal did not start".to_owned()))?
    };
    let sender = session
        .unique_name()
        .ok_or_else(|| zbus::Error::Failure("no name on the session bus".to_owned()))?;
    let (_, monitor) = portal_paths(sender.as_str(), MONITOR_TOKEN);
    // Subscribed before the monitor is asked for, so no state falls
    // between; the portal sends them to this client alone.
    let rule = zbus::MatchRule::builder()
        .msg_type(Type::Signal)
        .sender(owner.as_str())?
        .build();
    let signals = MessageIterator::for_match_rule(rule, session, None)?;
    let options = HashMap::from([
        ("handle_token", Value::from(MONITOR_TOKEN)),
        ("session_handle_token", Value::from(MONITOR_TOKEN)),
    ]);
    let inhibit = proxy(session, owner.as_str(), PORTAL_PATH, PORTAL_INHIBIT)?;
    let request: OwnedObjectPath = inhibit.call("CreateMonitor", &("", options))?;
    for signal in signals {
        let signal = signal?;
        let header = signal.header();
        let path = header.path().map(zbus::zvariant::ObjectPath::as_str);
        match header.member().map(zbus::names::MemberName::as_str) {
            Some("Response") if path == Some(request.as_str()) => {
                let (response, _): (u32, Options) = signal.body().deserialize()?;
                if response != 0 {
                    return Err(zbus::Error::Failure(format!(
                        "the portal refused the session monitor ({response})"
                    )));
                }
            }
            Some("StateChanged")
                if path == Some(PORTAL_PATH)
                    && header.interface().map(zbus::names::InterfaceName::as_str)
                        == Some(PORTAL_INHIBIT) =>
            {
                let (session_handle, state): (OwnedObjectPath, Options) =
                    signal.body().deserialize()?;
                // Another monitor's.
                if session_handle.as_str() != monitor {
                    continue;
                }
                let state = state
                    .get("session-state")
                    .and_then(|state| u32::try_from(state).ok());
                match monitor_step(state) {
                    // A lost answer leaves the client following: the end
                    // may still come.
                    MonitorStep::Answer => {
                        let answered =
                            inhibit.call::<_, _, ()>("QueryEndResponse", &(session_handle,));
                        if let Err(error) = answered {
                            tracing::warn!(%error, "the portal's query went unanswered");
                        }
                    }
                    MonitorStep::SaveQuit => {
                        tracing::info!("the portal reports the session ending; saving");
                        (on_end.save)();
                        (on_end.quit)();
                        return Ok(());
                    }
                    MonitorStep::Wait => {}
                }
            }
            _ => {}
        }
    }
    Err(zbus::Error::Failure("the session bus closed".to_owned()))
}

/// Whether the recorder is busy, as the shell last told the inhibitor's
/// client (`note_recording`), and the way to tell it. Managed state.
pub struct LogoutInhibitor {
    busy: Mutex<(mpsc::Sender<bool>, bool)>,
}

impl LogoutInhibitor {
    fn new(busy: mpsc::Sender<bool>) -> Self {
        Self {
            busy: Mutex::new((busy, false)),
        }
    }

    /// Tells the client `busy` when it changed; false when the client has
    /// gone.
    fn note(&self, busy: bool) -> bool {
        let Ok(mut noted) = self.busy.lock() else {
            return false;
        };
        if noted.1 == busy {
            return true;
        }
        noted.1 = busy;
        noted.0.send(busy).is_ok()
    }
}

/// The recorder's state the shell follows (`bridge::emit`): the logout
/// inhibitor is held while it is busy.
pub fn note_recording(app: &tauri::AppHandle, state: crate::recording::RecordingState) {
    use crate::recording::RecorderState as _;
    if let Some(inhibitor) = app.try_state::<LogoutInhibitor>() {
        inhibitor.note(state.is_busy());
    }
}

/// Holds the portal's logout inhibitor (`Inhibit` with the `Logout` flag
/// and `INHIBIT_REASON`) while the last state `busy` brought was busy, and
/// releases it (`Close` on its request) when it turns idle. A desktop that
/// honours it asks the user before a logout while a recording runs (GNOME,
/// through gnome-session, even for `gnome-session-quit --no-prompt`);
/// Plasma notes it for its session monitor; one that does not (the GTK
/// portal off GNOME: Xfce, wlroots) refuses it. Either way the save at the
/// end is the same. A call that fails is logged and tried again at the
/// next recording; returns once `busy` has no sender.
fn hold_logout_inhibitor(session: &Connection, busy: &mpsc::Receiver<bool>) {
    let mut held: Option<OwnedObjectPath> = None;
    let mut calls = 0_u32;
    for busy in busy {
        match (busy, held.take()) {
            (true, None) => {
                calls += 1;
                let options = HashMap::from([
                    ("handle_token", Value::from(format!("steno_logout_{calls}"))),
                    ("reason", Value::from(INHIBIT_REASON)),
                ]);
                match proxy(session, PORTAL, PORTAL_PATH, PORTAL_INHIBIT)
                    .and_then(|inhibit| inhibit.call("Inhibit", &("", INHIBIT_LOGOUT, options)))
                {
                    Ok(request) => held = Some(request),
                    Err(error) => tracing::info!(%error, "a recording does not hold a logout back"),
                }
            }
            (false, Some(request)) => {
                let closed = session.call_method(
                    Some(PORTAL),
                    request.as_str(),
                    Some(PORTAL_REQUEST),
                    "Close",
                    &(),
                );
                if let Err(error) = closed {
                    tracing::debug!(%error, "the logout inhibitor was gone already");
                }
            }
            (_, kept) => held = kept,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, Read as _};

    /// GNOME's query is answered at once, Xfce's saved for first; the end
    /// saves on both.
    #[test]
    fn the_session_managers_signals_map_to_their_steps() {
        assert_eq!(
            client_step(GNOME_SESSION, "QueryEndSession"),
            ClientStep::Answer
        );
        assert_eq!(
            client_step(XFCE4_SESSION, "QueryEndSession"),
            ClientStep::SaveAnswerQuit
        );
        for api in [GNOME_SESSION, XFCE4_SESSION] {
            assert_eq!(client_step(api, "EndSession"), ClientStep::SaveAnswerQuit);
            assert_eq!(client_step(api, "Stop"), ClientStep::Quit);
            assert_eq!(client_step(api, "CancelEndSession"), ClientStep::Wait);
            assert_eq!(client_step(api, "Unknown"), ClientStep::Wait);
        }
    }

    /// The portal's query-end is answered and its end saves; running,
    /// unknown and missing states wait.
    #[test]
    fn the_portals_session_states_map_to_their_steps() {
        assert_eq!(monitor_step(Some(2)), MonitorStep::Answer);
        assert_eq!(monitor_step(Some(3)), MonitorStep::SaveQuit);
        for state in [None, Some(0), Some(1), Some(4)] {
            assert_eq!(monitor_step(state), MonitorStep::Wait, "{state:?}");
        }
    }

    /// The portal names a request and a session after the caller's unique
    /// name, its dots and colon dropped, and the token.
    #[test]
    fn the_portal_names_its_objects_after_the_caller_and_the_token() {
        assert_eq!(
            portal_paths(":1.42", "steno"),
            (
                "/org/freedesktop/portal/desktop/request/1_42/steno".to_owned(),
                "/org/freedesktop/portal/desktop/session/1_42/steno".to_owned()
            )
        );
    }

    /// A private bus: `dbus-daemon` on a socket of its own, ended with the
    /// value.
    struct Daemon {
        child: std::process::Child,
        address: String,
    }

    impl Daemon {
        /// None when `dbus-daemon` is not installed, unless
        /// `STENO_REQUIRE_DBUS_TEST` asks for it (CI on Linux), which fails
        /// the test instead.
        fn start() -> Option<Self> {
            let spawned = std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
                .arg(format!(
                    "--address=unix:tmpdir={}",
                    std::env::temp_dir().display()
                ))
                .stdout(std::process::Stdio::piped())
                .spawn();
            let mut child = match spawned {
                Ok(child) => child,
                Err(error) => {
                    assert!(
                        std::env::var_os("STENO_REQUIRE_DBUS_TEST").is_none(),
                        "dbus-daemon did not start: {error}"
                    );
                    eprintln!("SKIPPED: dbus-daemon did not start: {error}");
                    return None;
                }
            };
            let mut address = String::new();
            std::io::BufReader::new(child.stdout.take().expect("piped"))
                .read_line(&mut address)
                .expect("the daemon's address");
            Some(Self {
                child,
                address: address.trim().to_owned(),
            })
        }

        fn builder(&self) -> Builder<'_> {
            Builder::address(self.address.as_str()).unwrap()
        }

        fn connect(&self) -> Connection {
            patient(self.builder()).unwrap()
        }
    }

    impl Daemon {
        /// Ends the bus, and with it every connection to it.
        fn end(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            self.end();
        }
    }

    type Steps = Arc<Mutex<Vec<String>>>;

    /// A `SaveAndQuit` that notes its steps; `save` takes a moment, so a
    /// lock released or an answer sent before it ended would be noted
    /// first.
    fn noting(steps: &Steps) -> SaveAndQuit {
        let (saving, quitting) = (steps.clone(), steps.clone());
        SaveAndQuit {
            save: Arc::new(move || {
                std::thread::sleep(SAVE);
                saving.lock().unwrap().push("saved".to_owned());
            }),
            quit: Arc::new(move || quitting.lock().unwrap().push("quit".to_owned())),
        }
    }

    /// How long `noting`'s save takes.
    const SAVE: Duration = Duration::from_millis(200);

    /// Runs `client` on a thread; its result arrives on the receiver.
    fn spawn(
        client: impl FnOnce() -> zbus::Result<()> + Send + 'static,
    ) -> mpsc::Receiver<zbus::Result<()>> {
        let (done, result) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(client());
        });
        result
    }

    const WAIT: Duration = Duration::from_secs(10);

    /// logind's manager as far as the lock goes: `Inhibit` hands out the
    /// write end of a pipe and the test keeps the read end, which reads to
    /// its end once the client released the lock.
    struct FakeLogind {
        /// `Inhibit`'s four arguments and the lock's read end.
        inhibited: mpsc::Sender<([String; 4], std::io::PipeReader)>,
    }

    /// The lock's arguments and its read end, as `Inhibit` hands it out.
    type Inhibits = mpsc::Receiver<([String; 4], std::io::PipeReader)>;

    impl FakeLogind {
        /// Serves logind's names on `daemon`, its locks arriving on the
        /// receiver.
        fn serve(daemon: &Daemon) -> (Connection, Inhibits) {
            let (inhibited, inhibits) = mpsc::channel();
            let logind = daemon
                .builder()
                .name(LOGIND)
                .unwrap()
                .serve_at(LOGIND_PATH, Self { inhibited })
                .unwrap()
                .build()
                .unwrap();
            (logind, inhibits)
        }
    }

    #[zbus::interface(name = "org.freedesktop.login1.Manager")]
    impl FakeLogind {
        fn inhibit(
            &self,
            what: String,
            who: String,
            why: String,
            mode: String,
        ) -> zbus::fdo::Result<OwnedFd> {
            let (reader, writer) =
                std::io::pipe().map_err(|error| zbus::fdo::Error::IOError(error.to_string()))?;
            let _ = self.inhibited.send(([what, who, why, mode], reader));
            Ok(std::os::fd::OwnedFd::from(writer).into())
        }
    }

    /// The client takes the `shutdown` delay lock, does nothing on a
    /// shutdown called off, and on a shutdown saves, releases the lock
    /// after the save, and quits.
    #[test]
    fn a_shutdown_saves_before_the_lock_is_released_and_then_quits() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let (logind, inhibits) = FakeLogind::serve(&daemon);
        let steps = Steps::default();
        let (client, on_end) = (daemon.connect(), noting(&steps));
        let result = spawn(move || hold_shutdown_lock(&client, &on_end));

        let (arguments, mut lock) = inhibits.recv_timeout(WAIT).expect("the lock was taken");
        assert_eq!(
            arguments,
            [
                "shutdown",
                "Steno",
                "Saves the recording in progress",
                "delay"
            ]
        );
        let released = {
            let steps = steps.clone();
            std::thread::spawn(move || {
                let mut rest = Vec::new();
                let _ = lock.read_to_end(&mut rest);
                steps.lock().unwrap().push("released".to_owned());
            })
        };
        let emit = |starting: bool| {
            logind
                .emit_signal(
                    None::<&str>,
                    LOGIND_PATH,
                    LOGIND_MANAGER,
                    "PrepareForShutdown",
                    &(starting,),
                )
                .unwrap();
        };
        emit(false);
        // Longer than a save, so a client that acted on it would be seen.
        std::thread::sleep(SAVE * 2);
        assert!(steps.lock().unwrap().is_empty(), "{steps:?}");
        assert!(result.try_recv().is_err(), "the client ended");
        emit(true);
        result
            .recv_timeout(WAIT)
            .expect("the client ended")
            .unwrap();
        released.join().unwrap();
        let steps = steps.lock().unwrap().clone();
        assert_eq!(steps.iter().filter(|step| *step == "saved").count(), 1);
        let at = |step: &str| steps.iter().position(|noted| noted == step).unwrap();
        assert!(at("saved") < at("released"), "{steps:?}");
        assert!(at("saved") < at("quit"), "{steps:?}");
    }

    /// The system bus going away while the client holds the lock ends it
    /// with an error, and it neither saves nor quits.
    #[test]
    fn a_shutdown_client_whose_bus_goes_away_ends_with_an_error() {
        let Some(mut daemon) = Daemon::start() else {
            return;
        };
        let (logind, inhibits) = FakeLogind::serve(&daemon);
        let logind = logind.unique_name().unwrap().to_string();
        let steps = Steps::default();
        let (client, on_end) = (daemon.connect(), noting(&steps));
        // Every message the client receives, logind's answer among them.
        let received = MessageIterator::from(&client);
        let result = spawn(move || hold_shutdown_lock(&client, &on_end));
        inhibits.recv_timeout(WAIT).expect("the lock was taken");
        // Once the answer reached the client, it holds the lock and waits
        // for a shutdown, so the bus ends under that wait.
        received
            .into_iter()
            .find(|message| {
                message.as_ref().is_ok_and(|message| {
                    message.message_type() == Type::MethodReturn
                        && message
                            .header()
                            .sender()
                            .map(zbus::names::UniqueName::as_str)
                            == Some(logind.as_str())
                })
            })
            .expect("logind answered")
            .unwrap();
        daemon.end();
        let ended = result.recv_timeout(WAIT).expect("the client ended");
        assert!(ended.is_err(), "{ended:?}");
        assert!(steps.lock().unwrap().is_empty(), "{steps:?}");
    }

    /// The names gnome-session and xfce4-session serve, written out apart
    /// from `SessionApi`'s so that a wrong name there fails the tests.
    const GNOME_SESSION: SessionApi = SessionApi {
        name: "org.gnome.SessionManager",
        path: "/org/gnome/SessionManager",
        manager: "org.gnome.SessionManager",
        client: "org.gnome.SessionManager.ClientPrivate",
        saves_at_query: false,
    };
    const XFCE4_SESSION: SessionApi = SessionApi {
        name: "org.xfce.SessionManager",
        path: "/org/xfce/SessionManager",
        manager: "org.xfce.Session.Manager",
        client: "org.xfce.Session.Client",
        saves_at_query: true,
    };

    /// A session manager under `api`'s names as far as one client goes, on
    /// `manager`: it registers the client at `client` and notes the ids,
    /// and notes the client's answers. A call that names another object,
    /// interface or method goes unanswered.
    fn serve_session_manager(
        manager: &Connection,
        api: SessionApi,
        client: OwnedObjectPath,
        registered: mpsc::Sender<(String, String)>,
        steps: Steps,
        answered: mpsc::Sender<()>,
    ) {
        let (manager, calls) = (manager.clone(), MessageIterator::from(manager));
        std::thread::spawn(move || {
            for call in calls {
                let Ok(call) = call else {
                    return;
                };
                let header = call.header();
                let to = (
                    header.path().map(zbus::zvariant::ObjectPath::as_str),
                    header.interface().map(zbus::names::InterfaceName::as_str),
                );
                match header.member().map(zbus::names::MemberName::as_str) {
                    Some("RegisterClient") if to == (Some(api.path), Some(api.manager)) => {
                        let _ = registered.send(call.body().deserialize().unwrap());
                        manager.reply(&header, &client).unwrap();
                    }
                    Some("EndSessionResponse")
                        if to == (Some(client.as_str()), Some(api.client)) =>
                    {
                        let (is_ok, reason): (bool, String) = call.body().deserialize().unwrap();
                        // "answered true" for the yes without a reason the
                        // client gives.
                        let step = [format!("answered {is_ok}"), reason].concat();
                        steps.lock().unwrap().push(step);
                        manager.reply(&header, &()).unwrap();
                        let _ = answered.send(());
                    }
                    _ => {}
                }
            }
        });
    }

    /// A fake session manager under `api`'s names on a private bus, with a
    /// client following it that has registered.
    struct Session {
        api: SessionApi,
        /// The client's object.
        client: String,
        /// The client's unique name on the bus.
        client_name: String,
        manager: Connection,
        steps: Steps,
        answers: mpsc::Receiver<()>,
        result: mpsc::Receiver<zbus::Result<()>>,
    }

    impl Session {
        fn follow(daemon: &Daemon, api: SessionApi) -> Self {
            let steps = Steps::default();
            let (registered, registrations) = mpsc::channel();
            let (answered, answers) = mpsc::channel();
            let client = format!("{}/Client1", api.path);
            let manager = daemon.builder().name(api.name).unwrap().build().unwrap();
            serve_session_manager(
                &manager,
                api,
                OwnedObjectPath::try_from(client.as_str()).unwrap(),
                registered,
                steps.clone(),
                answered,
            );
            let (connection, on_end) = (daemon.connect(), noting(&steps));
            let client_name = connection.unique_name().unwrap().to_string();
            let result = spawn(move || follow_session_end(&connection, "a-startup-id", &on_end));
            assert_eq!(
                registrations.recv_timeout(WAIT).expect("registered"),
                (APP_ID.to_owned(), "a-startup-id".to_owned())
            );
            Self {
                api,
                client,
                client_name,
                manager,
                steps,
                answers,
                result,
            }
        }

        /// The manager's signal `member` to every client on `path`.
        fn emit(&self, path: &str, member: &str) {
            self.manager
                .emit_signal(None::<&str>, path, self.api.client, member, &(0_u32,))
                .unwrap();
        }

        fn steps(&self) -> Vec<String> {
            self.steps.lock().unwrap().clone()
        }

        /// Asks the client whether the session may end and waits for its
        /// yes, which it gives without saving.
        fn query(&self) {
            self.emit(&self.client, "QueryEndSession");
            self.answers
                .recv_timeout(WAIT)
                .expect("the query was answered");
            assert_eq!(self.steps(), ["answered true"]);
        }

        /// Asks the client whether the session may end, where the query
        /// comes only once it ends: the client saves, answers and quits, in
        /// that order.
        fn query_ends(&self) {
            self.emit(&self.client, "QueryEndSession");
            self.result
                .recv_timeout(WAIT)
                .expect("the client ended")
                .unwrap();
            assert_eq!(self.steps(), ["saved", "answered true", "quit"]);
        }

        /// Ends the session: the client saves, answers and quits, in that
        /// order, after the one query.
        fn end(&self) {
            self.emit(&self.client, "EndSession");
            self.result
                .recv_timeout(WAIT)
                .expect("the client ended")
                .unwrap();
            assert_eq!(
                self.steps(),
                ["answered true", "saved", "answered true", "quit"]
            );
        }
    }

    /// The client registers with the app's id and its startup id, ignores
    /// another client's signals, and saves, answers and quits, in that
    /// order: on GNOME at the session's end, after a query it answered at
    /// once without saving; on Xfce at the query.
    fn a_logout_saves_before_the_session_manager_is_answered(api: SessionApi) {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let session = Session::follow(&daemon, api);
        session.emit(&format!("{}/Client2", api.path), "EndSession");
        if api.saves_at_query {
            session.query_ends();
        } else {
            session.query();
            session.end();
        }
    }

    #[test]
    fn a_gnome_logout_saves_before_the_session_manager_is_answered() {
        a_logout_saves_before_the_session_manager_is_answered(GNOME_SESSION);
    }

    #[test]
    fn an_xfce_logout_saves_before_the_session_manager_is_answered() {
        a_logout_saves_before_the_session_manager_is_answered(XFCE4_SESSION);
    }

    /// A peer other than the session manager that sends the client's
    /// signals, to every client or straight to the app, is ignored.
    #[test]
    fn a_session_signal_from_another_peer_is_ignored() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let session = Session::follow(&daemon, GNOME_SESSION);
        let peer = daemon.connect();
        for destination in [None, Some(session.client_name.as_str())] {
            peer.emit_signal(
                destination,
                session.client.as_str(),
                session.api.client,
                "EndSession",
                &(0_u32,),
            )
            .unwrap();
        }
        // The bus routes the peer's messages in order, so once it answered
        // this call it has passed both signals on, ahead of the query.
        peer.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetId",
            &(),
        )
        .unwrap();
        session.query();
        session.end();
    }

    /// With GNOME's and Xfce's names both on the bus, the client registers
    /// with GNOME's only.
    #[test]
    fn a_client_registers_with_gnomes_session_manager_before_xfces() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let (xfce_registered, xfce_registrations) = mpsc::channel();
        let (xfce_answered, _xfce_answers) = mpsc::channel();
        let xfce = daemon
            .builder()
            .name(XFCE4_SESSION.name)
            .unwrap()
            .build()
            .unwrap();
        serve_session_manager(
            &xfce,
            XFCE4_SESSION,
            OwnedObjectPath::try_from("/org/xfce/SessionManager/Client1").unwrap(),
            xfce_registered,
            Steps::default(),
            xfce_answered,
        );
        let session = Session::follow(&daemon, GNOME_SESSION);
        session.query();
        session.end();
        assert!(
            xfce_registrations.try_recv().is_err(),
            "registered with Xfce's"
        );
    }

    /// The session bus going away while the client follows the session
    /// manager ends it with an error, and it neither saves nor quits.
    #[test]
    fn a_session_client_whose_bus_goes_away_ends_with_an_error() {
        let Some(mut daemon) = Daemon::start() else {
            return;
        };
        let session = Session::follow(&daemon, GNOME_SESSION);
        // Answered, so the client follows the signals and the bus ends
        // under that wait.
        session.query();
        daemon.end();
        let ended = session.result.recv_timeout(WAIT).expect("the client ended");
        assert!(ended.is_err(), "{ended:?}");
        assert_eq!(session.steps(), ["answered true"]);
    }

    /// No session manager and no logind on the bus: both clients end with
    /// an error, and neither saves nor quits.
    #[test]
    fn without_the_services_neither_client_saves_nor_quits() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let on_end = SaveAndQuit {
            save: Arc::new(|| panic!("saved")),
            quit: Arc::new(|| panic!("quit")),
        };
        assert!(follow_session_end(&daemon.connect(), "", &on_end).is_err());
        assert!(hold_shutdown_lock(&daemon.connect(), &on_end).is_err());
    }

    /// What the fake portal was asked.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum PortalCall {
        /// The caller's unique name and the monitor's two tokens.
        CreateMonitor {
            window: String,
            sender: String,
            handle_token: String,
            session_handle_token: String,
        },
        /// The arguments, the reason and the request it handed out.
        Inhibit {
            window: String,
            flags: u32,
            reason: String,
            request: String,
        },
        /// A request closed.
        Closed(String),
    }

    fn option(options: &Options, key: &str) -> String {
        options
            .get(key)
            .and_then(|value| <&str>::try_from(value).ok())
            .unwrap_or_default()
            .to_owned()
    }

    /// The desktop portal's inhibitor as far as the app goes: it notes the
    /// monitors and inhibitors asked of it on `calls`, the answers to a
    /// query in `steps` ("answered <session>"), and serves each inhibitor's request
    /// until it is closed. The states and the responses are the test's to
    /// send (`state`).
    struct FakePortal {
        calls: mpsc::Sender<PortalCall>,
        steps: Steps,
    }

    impl FakePortal {
        fn serve(daemon: &Daemon, steps: &Steps) -> (Connection, mpsc::Receiver<PortalCall>) {
            let (calls, called) = mpsc::channel();
            let portal = daemon
                .builder()
                .name(PORTAL)
                .unwrap()
                .serve_at(
                    PORTAL_PATH,
                    Self {
                        calls,
                        steps: steps.clone(),
                    },
                )
                .unwrap()
                .build()
                .unwrap();
            (portal, called)
        }
    }

    // The interface macro hands every argument over by value.
    #[allow(clippy::needless_pass_by_value)]
    #[zbus::interface(name = "org.freedesktop.portal.Inhibit")]
    impl FakePortal {
        fn create_monitor(
            &self,
            window: String,
            options: Options,
            #[zbus(header)] header: zbus::message::Header<'_>,
        ) -> OwnedObjectPath {
            let sender = header.sender().map(ToString::to_string).unwrap_or_default();
            let token = option(&options, "handle_token");
            let (request, _) = portal_paths(&sender, &token);
            let _ = self.calls.send(PortalCall::CreateMonitor {
                window,
                sender,
                handle_token: token,
                session_handle_token: option(&options, "session_handle_token"),
            });
            OwnedObjectPath::try_from(request).unwrap()
        }

        fn query_end_response(&self, session_handle: OwnedObjectPath) {
            let step = format!("answered {}", session_handle.as_str());
            self.steps.lock().unwrap().push(step);
        }

        async fn inhibit(
            &self,
            window: String,
            flags: u32,
            options: Options,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(object_server)] server: &zbus::ObjectServer,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            let sender = header.sender().map(ToString::to_string).unwrap_or_default();
            let (request, _) = portal_paths(&sender, &option(&options, "handle_token"));
            server
                .at(
                    request.as_str(),
                    FakeRequest {
                        path: request.clone(),
                        calls: self.calls.clone(),
                    },
                )
                .await?;
            let _ = self.calls.send(PortalCall::Inhibit {
                window,
                flags,
                reason: option(&options, "reason"),
                request: request.clone(),
            });
            Ok(OwnedObjectPath::try_from(request).unwrap())
        }
    }

    /// One of the fake portal's requests, which notes its `Close`.
    struct FakeRequest {
        path: String,
        calls: mpsc::Sender<PortalCall>,
    }

    #[zbus::interface(name = "org.freedesktop.portal.Request")]
    impl FakeRequest {
        async fn close(&self, #[zbus(object_server)] server: &zbus::ObjectServer) {
            let _ = self.calls.send(PortalCall::Closed(self.path.clone()));
            let _ = server.remove::<Self, _>(self.path.as_str()).await;
        }
    }

    /// The portal's `session-state` for `session` to `client`, as the
    /// portal sends it, from `from`.
    fn state(from: &Connection, client: &str, session: &str, state: u32) {
        state_on(from, client, (PORTAL_PATH, PORTAL_INHIBIT), session, state);
    }

    /// `state`, sent on another object or interface than the portal's.
    fn state_on(
        from: &Connection,
        client: &str,
        (path, interface): (&str, &str),
        session: &str,
        state: u32,
    ) {
        let fields = HashMap::from([
            ("screensaver-active", Value::from(false)),
            ("session-state", Value::from(state)),
        ]);
        from.emit_signal(
            Some(client),
            path,
            interface,
            "StateChanged",
            &(OwnedObjectPath::try_from(session).unwrap(), fields),
        )
        .unwrap();
    }

    /// A monitor client following the fake portal, with no session
    /// manager on the bus, once the portal answered its monitor with
    /// `response`.
    struct Monitor {
        portal: Connection,
        client: String,
        session: String,
        steps: Steps,
        result: mpsc::Receiver<zbus::Result<()>>,
    }

    impl Monitor {
        fn follow(daemon: &Daemon, response: u32) -> Self {
            let steps = Steps::default();
            let (portal, calls) = FakePortal::serve(daemon, &steps);
            let (connection, on_end) = (daemon.connect(), noting(&steps));
            let client = connection.unique_name().unwrap().to_string();
            let result = spawn(move || follow_session_end(&connection, "", &on_end));
            let asked = calls.recv_timeout(WAIT);
            assert!(
                asked.is_ok(),
                "no monitor was asked for: {:?}",
                result.try_recv()
            );
            assert_eq!(
                asked.unwrap(),
                PortalCall::CreateMonitor {
                    window: String::new(),
                    sender: client.clone(),
                    handle_token: MONITOR_TOKEN.to_owned(),
                    session_handle_token: MONITOR_TOKEN.to_owned(),
                }
            );
            let (request, session) = portal_paths(&client, MONITOR_TOKEN);
            let results = HashMap::from([("session_handle", Value::from(session.as_str()))]);
            portal
                .emit_signal(
                    Some(client.as_str()),
                    request.as_str(),
                    PORTAL_REQUEST,
                    "Response",
                    &(response, results),
                )
                .unwrap();
            Self {
                portal,
                client,
                session,
                steps,
                result,
            }
        }

        fn steps(&self) -> Vec<String> {
            self.steps.lock().unwrap().clone()
        }

        /// Nothing happened, for longer than a save takes.
        fn waits(&self) {
            std::thread::sleep(SAVE * 2);
            assert!(self.steps().is_empty(), "{:?}", self.steps());
            assert!(self.result.try_recv().is_err(), "the client ended");
        }

        fn ended(&self) -> zbus::Result<()> {
            self.result.recv_timeout(WAIT).expect("the client ended")
        }

        /// Waits until the steps are `wanted`, `WAIT` at most.
        fn reaches(&self, wanted: &[&str]) {
            let deadline = std::time::Instant::now() + WAIT;
            while self.steps() != wanted && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(self.steps(), wanted);
        }
    }

    /// Without a session manager the client opens the portal's session
    /// monitor with its tokens and waits while the session runs, through
    /// another monitor's states and through a response or a state on
    /// another object or interface. It answers the query at once without
    /// saving and goes on recording when the end is called off; at the end
    /// it saves and quits, in that order.
    #[test]
    fn a_portal_query_is_answered_at_once_and_the_end_saves_then_quits() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let monitor = Monitor::follow(&daemon, 0);
        let (portal, client, session) = (&monitor.portal, &monitor.client, &monitor.session);
        let (other_request, other) = portal_paths(":1.999", MONITOR_TOKEN);
        state(portal, client, session, 1);
        state(portal, client, &other, 2);
        state(portal, client, &other, 3);
        // A refusal on another request would end the client.
        let refused = HashMap::from([("session_handle", Value::from(session.as_str()))]);
        portal
            .emit_signal(
                Some(client.as_str()),
                other_request.as_str(),
                PORTAL_REQUEST,
                "Response",
                &(2_u32, refused),
            )
            .unwrap();
        let elsewhere = format!("{PORTAL_PATH}/elsewhere");
        state_on(portal, client, (&elsewhere, PORTAL_INHIBIT), session, 3);
        state_on(portal, client, (PORTAL_PATH, PORTAL_REQUEST), session, 3);
        monitor.waits();
        state(portal, client, session, 2);
        let answered = format!("answered {session}");
        monitor.reaches(&[answered.as_str()]);
        // The logout was called off: the session runs again.
        state(portal, client, session, 1);
        std::thread::sleep(SAVE * 2);
        assert_eq!(monitor.steps(), [answered.as_str()]);
        assert!(monitor.result.try_recv().is_err(), "the client ended");
        state(portal, client, session, 3);
        monitor.ended().unwrap();
        assert_eq!(monitor.steps(), [answered.as_str(), "saved", "quit"]);
    }

    /// At the session's end the client saves and quits; there is no query
    /// to answer.
    #[test]
    fn a_portal_end_saves_and_then_quits() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let monitor = Monitor::follow(&daemon, 0);
        state(&monitor.portal, &monitor.client, &monitor.session, 3);
        monitor.ended().unwrap();
        assert_eq!(monitor.steps(), ["saved", "quit"]);
    }

    /// A peer other than the portal that sends the client a state is
    /// ignored.
    #[test]
    fn a_session_state_from_another_peer_is_ignored() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let monitor = Monitor::follow(&daemon, 0);
        let peer = daemon.connect();
        state(&peer, &monitor.client, &monitor.session, 3);
        monitor.waits();
        state(&monitor.portal, &monitor.client, &monitor.session, 3);
        monitor.ended().unwrap();
        assert_eq!(monitor.steps(), ["saved", "quit"]);
    }

    /// A monitor the portal refuses ends the client with an error, and a
    /// state after it saves nothing.
    #[test]
    fn a_refused_monitor_ends_the_client_with_an_error() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let monitor = Monitor::follow(&daemon, 2);
        let ended = monitor.ended();
        assert!(ended.is_err(), "{ended:?}");
        state(&monitor.portal, &monitor.client, &monitor.session, 3);
        std::thread::sleep(SAVE * 2);
        assert!(monitor.steps().is_empty(), "{:?}", monitor.steps());
    }

    /// The inhibitor is taken once when the recorder turns busy, with the
    /// `Logout` flag and the reason, released when it turns idle, and taken
    /// anew for the next recording; a state that did not change asks for
    /// nothing.
    #[test]
    fn the_logout_inhibitor_is_held_only_while_recording() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let (_portal, calls) = FakePortal::serve(&daemon, &Steps::default());
        let client = daemon.connect();
        let name = client.unique_name().unwrap().to_string();
        let (busy, recording) = mpsc::channel();
        let inhibitor = LogoutInhibitor::new(busy);
        let result = spawn(move || {
            hold_logout_inhibitor(&client, &recording);
            Ok(())
        });
        let quiet = || {
            std::thread::sleep(SAVE);
            assert_eq!(calls.try_recv(), Err(mpsc::TryRecvError::Empty));
        };
        let taken = |call: usize| {
            let (request, _) = portal_paths(&name, &format!("steno_logout_{call}"));
            assert_eq!(
                calls.recv_timeout(WAIT).expect("the inhibitor was taken"),
                PortalCall::Inhibit {
                    window: String::new(),
                    flags: 1,
                    reason: INHIBIT_REASON.to_owned(),
                    request: request.clone(),
                }
            );
            request
        };
        assert!(inhibitor.note(false));
        quiet();
        assert!(inhibitor.note(true));
        let first = taken(1);
        assert!(inhibitor.note(true));
        quiet();
        assert!(inhibitor.note(false));
        assert_eq!(
            calls
                .recv_timeout(WAIT)
                .expect("the inhibitor was released"),
            PortalCall::Closed(first)
        );
        assert!(inhibitor.note(false));
        quiet();
        assert!(inhibitor.note(true));
        taken(2);
        drop(inhibitor);
        result
            .recv_timeout(WAIT)
            .expect("the client ended")
            .unwrap();
    }
}
