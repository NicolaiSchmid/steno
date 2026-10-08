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
//!   that name only. It answers `QueryEndSession` at once, and on
//!   `EndSession` saves, answers `EndSessionResponse` and quits:
//!   gnome-session asks before its confirmation dialog and gives a query
//!   one second, and the user can still call the logout off there
//!   (`CancelEndSession`); on X11 another client can still call an Xfce
//!   logout off after the query, and xfce4-session then sends the app no
//!   signal at all. Either way a recording goes on until the end really
//!   comes. gnome-session waits about ten seconds for the answer at the
//!   end, xfce4-session seven. `Stop` saves and quits: xfce4-session sends
//!   it for Session settings' Quit Program and kills the process 15
//!   seconds later. It also sends it to a client it has just dropped
//!   (`StateChanged` to disconnected) once a checkpoint ("Save Session")
//!   or a query's answer has waited a minute, with no logout to follow and
//!   no kill; that client records on and registers again
//!   (`Phase::Dropped`). Xfce on Wayland is the exception: xfce4-session
//!   quits right after the query, with no `EndSession` and no cancel to
//!   follow, so there the app saves at the query and answers; the display
//!   closing then ends it. Should the session go on after all (an X11
//!   session taken for a Wayland one, `wayland_session`), the app tells
//!   the user and relaunches (`SaveAndQuit::of`). Where no session manager
//!   runs (KDE Plasma, wlroots desktops), through the desktop portal's
//!   session monitor (`follow_portal`): the app opens it (`CreateMonitor` on
//!   `org.freedesktop.portal.Inhibit`), answers query-end at once
//!   (`QueryEndResponse`; the portal gives a second, and the end can still
//!   be called off), and at ending saves and quits. Plasma 6.6's portal
//!   serves the monitor, but nothing in Plasma 6.6 asks it yet, so it
//!   never reports the end there; Plasma before 6.6 and the GTK portal
//!   outside GNOME report no end.
//! - **The logout inhibitor** (`hold_logout_inhibitor`, told through
//!   `LogoutInhibitor`): while a recording runs the app holds the portal's
//!   `Inhibit` with the `Logout` flag. GNOME shows it in its logout
//!   dialog, Plasma 6.6's portal records it for its session monitor, which
//!   nothing in Plasma asks yet, and the GTK portal outside GNOME (Xfce,
//!   wlroots) refuses it.
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
//! first: systemd's `SIGKILL` once a stop has waited its timeout, 90 s by
//! default, or xfce4-session's 15 seconds after its `Stop`, which the
//! save's ten seconds fit in. On KDE Plasma, which does not ask the app,
//! that is the save.
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
use std::ffi::OsStr;
use std::os::unix::fs::FileTypeExt as _;
use std::path::Path;
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
    /// Whether the manager, on a Wayland session, ends it right after
    /// `QueryEndSession`, sends no `EndSession` and so can no longer call
    /// the logout off: the client saves at the query there, and at
    /// `EndSession` everywhere else (`client_step`).
    query_ends_on_wayland: bool,
}

impl SessionApi {
    const GNOME: Self = Self {
        name: "org.gnome.SessionManager",
        path: "/org/gnome/SessionManager",
        manager: "org.gnome.SessionManager",
        client: "org.gnome.SessionManager.ClientPrivate",
        // gnome-session asks before its confirmation dialog, which the user
        // can still cancel, and gives a query one second.
        query_ends_on_wayland: false,
    };
    const XFCE: Self = Self {
        name: "org.xfce.SessionManager",
        path: "/org/xfce/SessionManager",
        manager: "org.xfce.Session.Manager",
        client: "org.xfce.Session.Client",
        // On X11 xfce4-session waits up to a minute for the query's answer
        // and seven seconds after `EndSession`, and another client can still
        // call the logout off between the two (XSMP's interact-cancel); the
        // app's client is then set back to idle and hears nothing. A
        // checkpoint, and a query whose answer it refused because the
        // logout was called off first, end with that minute's save timeout,
        // which drops the client (`StateChanged` to disconnected) and sends
        // `Stop` though no logout follows and no kill comes. Session
        // settings' Quit Program sends `Stop` alone and kills the process
        // 15 seconds later (`kill_hung_client`). On Wayland it quits
        // right after sending the query
        // (`xfsm_manager_save_yourself_global`), with no `EndSession` and no
        // cancel to follow.
        query_ends_on_wayland: true,
    };
    /// In the order the app looks for them on the bus.
    const ALL: [Self; 2] = [Self::GNOME, Self::XFCE];
}

/// xfce4-session's client state `XFSM_CLIENT_DISCONNECTED`, the new state
/// its `StateChanged` names when it drops a client (`Phase::Dropped`).
const XFCE_CLIENT_DISCONNECTED: u32 = 7;

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
/// for the exit, which then goes through at once; `await_end`, after a
/// save at a query that should have been the session's end, returns at
/// once and starts the wait for that end.
#[derive(Clone)]
pub struct SaveAndQuit {
    save: Arc<dyn Fn() + Send + Sync>,
    quit: Arc<dyn Fn() + Send + Sync>,
    await_end: Arc<dyn Fn() + Send + Sync>,
}

/// How long the app, saved at a query that should have been the session's
/// end, waits for that end before it takes the logout as called off
/// (`SaveAndQuit::of`). xfce4-session on Wayland ends the session a second
/// or two after its query.
const END_AFTER_QUERY: Duration = Duration::from_secs(30);

/// What the app tells the user when the session went on after the save at
/// its query.
const CALLED_OFF_NOTICE: &str = "It looked like you were logging out, so Steno saved your \
    recording and stopped. You are still logged in, so Steno opens again when you close this \
    message, ready to record.";

impl SaveAndQuit {
    /// The app's: the save before an end (`save_before_end` in `main.rs`),
    /// then Quit. Its `await_end` waits `END_AFTER_QUERY` on a thread of
    /// its own: a session that really ends takes the process with it
    /// first. Once the wait is over the app, which saved and so records
    /// nothing more, tells the user and relaunches when the message is
    /// closed, so the user can record again, as after a logout called off.
    fn of(app: &tauri::AppHandle) -> Self {
        let (saving, quitting, waiting) = (app.clone(), app.clone(), app.clone());
        Self {
            save: Arc::new(move || crate::save_before_end(&saving)),
            quit: Arc::new(move || crate::actions::quit(&quitting)),
            await_end: Arc::new(move || {
                let app = waiting.clone();
                let spawned = std::thread::Builder::new()
                    .name("steno-session-end-wait".to_owned())
                    .spawn(move || {
                        std::thread::sleep(END_AFTER_QUERY);
                        tracing::warn!(
                            "the session went on after the save at its query; relaunching"
                        );
                        relaunch_after_notice(&app);
                    });
                if let Err(error) = spawned {
                    tracing::warn!(
                        %error,
                        "the app saved at the query and cannot wait for the session's end"
                    );
                }
            }),
        }
    }
}

/// Shows `CALLED_OFF_NOTICE` and relaunches the app however the message is
/// closed, through the exit request as the updater's relaunch goes
/// (`AppHandle::request_restart`); the shutdown already ran, so none runs
/// again.
fn relaunch_after_notice(app: &tauri::AppHandle) {
    use tauri_plugin_dialog::{DialogExt as _, MessageDialogButtons, MessageDialogKind};
    let relaunching = app.clone();
    app.dialog()
        .message(CALLED_OFF_NOTICE)
        .title("Steno")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCustom("Relaunch Steno".to_owned()))
        .show(move |_| relaunching.request_restart());
}

/// The startup id the session manager gave the app when it started it (an
/// autostart at login), so it knows the client; empty otherwise. Read
/// before Tauri builds the app: GTK unsets `DESKTOP_AUTOSTART_ID` when it
/// starts.
pub fn startup_id() -> String {
    std::env::var("DESKTOP_AUTOSTART_ID").unwrap_or_default()
}

/// Starts the three clients, the session's two only with a session bus
/// (the definition the single instance uses, `session_bus_named`);
/// `startup_id` is what `startup_id` read at launch.
pub fn watch(app: &tauri::AppHandle, startup_id: String) {
    let on_end = SaveAndQuit::of(app);
    if crate::session_bus_named() {
        let on_end = on_end.clone();
        spawn_client(
            "steno-session-client",
            "a logout saves only when a signal reaches the app or the display closes",
            move || {
                let session = patient(Builder::session()?)?;
                follow_session_end(&session, &startup_id, wayland_session(), &on_end)
            },
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

/// Whether the app runs in a Wayland session, also when the shell runs
/// under `XWayland`: `wayland_session_of` this process's environment.
/// `display.rs` asks only whether `WAYLAND_DISPLAY` is set, so for a stale
/// one it logs a Wayland session where this function finds X11.
fn wayland_session() -> bool {
    use std::env::var_os;
    wayland_session_of(
        var_os("XDG_SESSION_TYPE").as_deref(),
        var_os("WAYLAND_DISPLAY").as_deref(),
        var_os("XDG_RUNTIME_DIR").as_deref(),
    )
}

/// Whether a session with these `XDG_SESSION_TYPE`, `WAYLAND_DISPLAY` and
/// `XDG_RUNTIME_DIR` values is a Wayland one. The session type decides when
/// it is `wayland` or `x11`; otherwise a `WAYLAND_DISPLAY` decides whose
/// socket exists (the value itself when it is a path, else the name under
/// `XDG_RUNTIME_DIR`). An app started by the systemd user manager can
/// inherit an earlier session's `WAYLAND_DISPLAY`, whose compositor and
/// socket are gone. An empty value counts as none.
fn wayland_session_of(
    session_type: Option<&OsStr>,
    display: Option<&OsStr>,
    runtime_dir: Option<&OsStr>,
) -> bool {
    fn set(value: Option<&OsStr>) -> Option<&Path> {
        value.filter(|value| !value.is_empty()).map(Path::new)
    }
    match session_type.and_then(OsStr::to_str) {
        Some("wayland") => return true,
        Some("x11") => return false,
        _ => {}
    }
    let Some(display) = set(display) else {
        return false;
    };
    let socket = if display.is_absolute() {
        display.to_owned()
    } else if let Some(runtime_dir) = set(runtime_dir) {
        runtime_dir.join(display)
    } else {
        return false;
    };
    std::fs::metadata(socket).is_ok_and(|socket| socket.file_type().is_socket())
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

/// A proxy that caches no property: no client reads one, and the cache
/// would ask the bus for them.
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

/// Where a session client stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Registered, following the session manager; nothing saved yet.
    Running,
    /// The app saved at the query, which should have been the end, and
    /// answered it (`SaveAndQuit::await_end`).
    Saved,
    /// xfce4-session dropped the client (`StateChanged` to
    /// `XFCE_CLIENT_DISCONNECTED`) because a checkpoint or a query's answer
    /// waited out its minute; the `Stop` it sends next ends neither the
    /// session nor the app, and no kill follows it.
    Dropped,
}

/// What a session client does on one of the session manager's signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientStep {
    /// Answers yes at once: the logout can still be called off, and the
    /// logout inhibitor is what asks the user.
    Answer,
    /// Saves, answers and waits for the end (`SaveAndQuit::await_end`): the
    /// query is the session's end.
    SaveAnswer,
    /// Saves, answers, then quits: the session ends.
    SaveAnswerQuit,
    /// Answers, then quits: the session ends, and the app saved at the
    /// query.
    AnswerQuit,
    /// Saves, then quits: the session manager asks the app to leave, and
    /// xfce4-session kills it 15 seconds later.
    SaveQuit,
    /// Quits: the session manager asks the app to leave, and the app saved
    /// at the query.
    Quit,
    /// Registers again (`RegisterClient`) and records on: the session
    /// manager dropped the client, so a later logout or Quit Program would
    /// not reach it otherwise.
    Register,
    /// Nothing.
    Wait,
}

/// The step for the session manager's signal `member` in `phase`, and the
/// phase it leads to; `state` is the new state a `StateChanged` names. The
/// query is answered at once or, where it is the session's end
/// (`saves_at_query`, `SessionApi::query_ends_on_wayland`), after the save.
/// A query answered at once can still be called off (gnome-session sends
/// `CancelEndSession`, xfce4-session nothing), and the recording goes on.
/// `Stop` saves and quits, unless xfce4-session dropped the client just
/// before (`Phase::Dropped`): a checkpoint, or a query whose answer it
/// refused, timed out with no logout to follow. Should a later
/// xfce4-session drop a client without saying so, its `Stop` still saves
/// and quits, which loses nothing.
fn client_step(
    saves_at_query: bool,
    phase: Phase,
    member: &str,
    state: Option<u32>,
) -> (ClientStep, Phase) {
    match (member, phase) {
        ("QueryEndSession", Phase::Saved) => (ClientStep::Answer, phase),
        ("QueryEndSession", _) if saves_at_query => (ClientStep::SaveAnswer, Phase::Saved),
        ("QueryEndSession", _) => (ClientStep::Answer, phase),
        ("EndSession", Phase::Saved) => (ClientStep::AnswerQuit, phase),
        ("EndSession", _) => (ClientStep::SaveAnswerQuit, phase),
        ("Stop", Phase::Running) => (ClientStep::SaveQuit, phase),
        ("Stop", Phase::Saved) => (ClientStep::Quit, phase),
        ("Stop", Phase::Dropped) => (ClientStep::Register, Phase::Running),
        ("StateChanged", Phase::Running) if state == Some(XFCE_CLIENT_DISCONNECTED) => {
            (ClientStep::Wait, Phase::Dropped)
        }
        _ => (ClientStep::Wait, phase),
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

/// Follows the end of the session on `session`, a Wayland one when
/// `wayland`: as a client of its session manager when one runs
/// (`follow_session`), else through the desktop portal's session monitor
/// (`follow_portal`).
fn follow_session_end(
    session: &Connection,
    startup_id: &str,
    wayland: bool,
    on_end: &SaveAndQuit,
) -> zbus::Result<()> {
    match session_manager(session)? {
        Some(manager) => follow_session(session, manager, startup_id, wayland, on_end),
        None => follow_portal(session, on_end),
    }
}

/// Registers the app with `manager`, the session manager on `session`
/// (a Wayland one when `wayland`), and follows its signals to this client
/// (`client_step`, from `Phase::Running`) until the session ends; an error
/// when the bus goes away before the app quit, or the manager refuses to
/// register a client it dropped again.
fn follow_session(
    session: &Connection,
    (api, owner): (SessionApi, OwnedUniqueName),
    startup_id: &str,
    wayland: bool,
    on_end: &SaveAndQuit,
) -> zbus::Result<()> {
    let saves_at_query = api.query_ends_on_wayland && wayland;
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
    let register = || -> zbus::Result<OwnedObjectPath> {
        manager.call("RegisterClient", &(APP_ID, startup_id))
    };
    let mut client = register()?;
    let answer = |client: &OwnedObjectPath| {
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
    let unanswered = |error: zbus::Error| {
        tracing::warn!(%error, "the session manager's query went unanswered");
    };
    let mut phase = Phase::Running;
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
        // Only a `StateChanged` carries two states.
        let states = signal.body().deserialize::<(u32, u32)>();
        let state = states.ok().map(|(_, new)| new);
        let (step, next) = client_step(saves_at_query, phase, member.as_str(), state);
        phase = next;
        match step {
            // A lost or refused answer leaves the client following: the end
            // may still come, and xfce4-session refuses the answer to a
            // query called off first.
            ClientStep::Answer => answer(&client).unwrap_or_else(unanswered),
            // Saved, whatever became of the answer.
            ClientStep::SaveAnswer => {
                tracing::info!(signal = member.as_str(), "the session is ending; saving");
                (on_end.save)();
                answer(&client).unwrap_or_else(unanswered);
                (on_end.await_end)();
            }
            ClientStep::SaveAnswerQuit => {
                tracing::info!(signal = member.as_str(), "the session is ending; saving");
                (on_end.save)();
                let answered = answer(&client);
                (on_end.quit)();
                return answered;
            }
            ClientStep::AnswerQuit => {
                let answered = answer(&client);
                (on_end.quit)();
                return answered;
            }
            ClientStep::SaveQuit => {
                tracing::info!("the session manager asked the app to leave; saving");
                (on_end.save)();
                (on_end.quit)();
                return Ok(());
            }
            ClientStep::Quit => {
                tracing::info!("the session manager asked the app to leave; quitting");
                (on_end.quit)();
                return Ok(());
            }
            ClientStep::Register => {
                tracing::info!(
                    "the session manager dropped the app outside a logout; recording on \
                     and registering again"
                );
                client = register()?;
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

/// The shell's side of the logout inhibitor's client
/// (`hold_logout_inhibitor`): whether the recorder is busy, as the shell
/// last told the client (`note_recording`), and the way to tell it.
/// Managed state.
pub struct LogoutInhibitor(Mutex<Told>);

/// What `LogoutInhibitor` holds.
struct Told {
    /// The client's end of the channel.
    tell: mpsc::Sender<bool>,
    /// What the client was last told.
    busy: bool,
}

impl LogoutInhibitor {
    fn new(tell: mpsc::Sender<bool>) -> Self {
        Self(Mutex::new(Told { tell, busy: false }))
    }

    /// Tells the client `busy` when it changed; false when the client has
    /// gone.
    fn note(&self, busy: bool) -> bool {
        let Ok(mut told) = self.0.lock() else {
            return false;
        };
        if told.busy == busy {
            return true;
        }
        told.busy = busy;
        told.tell.send(busy).is_ok()
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
/// Plasma 6.6's portal records it for its session monitor, which nothing
/// in Plasma asks yet; the GTK portal outside GNOME (Xfce, wlroots)
/// refuses it. Either way the save at the end is the same. A call that
/// fails is logged and tried again at the next recording; returns once
/// `busy` has no sender.
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
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A query is answered at once unless it is the session's end, which
    /// saves first and then waits for the end; the end saves unless the
    /// query did; `Stop` saves and quits, unless xfce4-session dropped the
    /// client just before, which then registers again and records on.
    #[test]
    fn the_session_managers_signals_map_to_their_steps() {
        use ClientStep::{
            Answer, AnswerQuit, Quit, Register, SaveAnswer, SaveAnswerQuit, SaveQuit, Wait,
        };
        use Phase::{Dropped, Running, Saved};
        // Whether the client saves at the query (`saves_at_query`).
        const AT_QUERY: bool = true;
        const AT_END: bool = false;
        let dropped = Some(XFCE_CLIENT_DISCONNECTED);
        let cases = [
            (AT_END, Running, "QueryEndSession", None, (Answer, Running)),
            (AT_END, Dropped, "QueryEndSession", None, (Answer, Dropped)),
            (
                AT_QUERY,
                Running,
                "QueryEndSession",
                None,
                (SaveAnswer, Saved),
            ),
            (AT_QUERY, Saved, "QueryEndSession", None, (Answer, Saved)),
            (
                AT_END,
                Running,
                "EndSession",
                None,
                (SaveAnswerQuit, Running),
            ),
            (AT_QUERY, Saved, "EndSession", None, (AnswerQuit, Saved)),
            (AT_END, Running, "Stop", None, (SaveQuit, Running)),
            (AT_QUERY, Running, "Stop", None, (SaveQuit, Running)),
            (AT_QUERY, Saved, "Stop", None, (Quit, Saved)),
            (AT_END, Dropped, "Stop", None, (Register, Running)),
            (AT_END, Running, "StateChanged", dropped, (Wait, Dropped)),
            (AT_QUERY, Saved, "StateChanged", dropped, (Wait, Saved)),
            (AT_END, Running, "StateChanged", Some(0), (Wait, Running)),
            (AT_END, Running, "StateChanged", Some(3), (Wait, Running)),
            (AT_END, Running, "StateChanged", None, (Wait, Running)),
            (AT_END, Running, "CancelEndSession", None, (Wait, Running)),
            (AT_QUERY, Saved, "CancelEndSession", None, (Wait, Saved)),
            (AT_END, Running, "Unknown", dropped, (Wait, Running)),
            (AT_QUERY, Saved, "Unknown", None, (Wait, Saved)),
        ];
        for (saves_at_query, phase, member, state, step) in cases {
            assert_eq!(
                client_step(saves_at_query, phase, member, state),
                step,
                "{member} ({state:?}) in {phase:?}, saving at the query: {saves_at_query}"
            );
        }
    }

    /// The session type decides when it names Wayland or X11; otherwise a
    /// `WAYLAND_DISPLAY` whose socket exists, by name under the runtime
    /// directory or by path, makes the session a Wayland one, and an
    /// empty or stale one does not.
    #[test]
    fn a_wayland_session_is_told_by_its_type_or_its_live_socket() {
        let dir = std::env::temp_dir().join(format!("steno-wayland-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let live = dir.join("wayland-1");
        let _compositor = std::os::unix::net::UnixListener::bind(&live).unwrap();
        std::fs::write(dir.join("wayland-file"), "").unwrap();
        let runtime = Some(dir.as_os_str());
        let os = |value: &'static str| Some(OsStr::new(value));
        let cases = [
            (os("wayland"), None, None, true),
            (os("wayland"), os("wayland-9"), runtime, true),
            (os("x11"), os("wayland-1"), runtime, false),
            (None, os("wayland-1"), runtime, true),
            (os("tty"), os("wayland-1"), runtime, true),
            (os(""), os("wayland-1"), runtime, true),
            (None, Some(live.as_os_str()), None, true),
            (None, os("wayland-1"), None, false),
            (None, os("wayland-1"), os(""), false),
            (None, os("wayland-9"), runtime, false),
            (None, os("wayland-file"), runtime, false),
            (None, os(""), runtime, false),
            (None, None, runtime, false),
        ];
        for (session_type, display, runtime_dir, wayland) in cases {
            assert_eq!(
                wayland_session_of(session_type, display, runtime_dir),
                wayland,
                "{session_type:?} {display:?} {runtime_dir:?}"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
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
        let (saving, quitting, waiting) = (steps.clone(), steps.clone(), steps.clone());
        SaveAndQuit {
            save: Arc::new(move || {
                std::thread::sleep(SAVE);
                saving.lock().unwrap().push("saved".to_owned());
            }),
            quit: Arc::new(move || quitting.lock().unwrap().push("quit".to_owned())),
            await_end: Arc::new(move || waiting.lock().unwrap().push("awaits the end".to_owned())),
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

    /// Waits until `steps` are `wanted`, `WAIT` at most.
    fn reach(steps: &Steps, wanted: &[&str]) {
        let deadline = std::time::Instant::now() + WAIT;
        while *steps.lock().unwrap() != wanted && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(*steps.lock().unwrap(), wanted);
    }

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
        query_ends_on_wayland: false,
    };
    const XFCE4_SESSION: SessionApi = SessionApi {
        name: "org.xfce.SessionManager",
        path: "/org/xfce/SessionManager",
        manager: "org.xfce.Session.Manager",
        client: "org.xfce.Session.Client",
        query_ends_on_wayland: true,
    };

    /// A session manager under `api`'s names as far as one client goes, on
    /// `manager`: it registers the client at `client` and notes the ids,
    /// and notes the client's answers, refusing the next one when
    /// `refuse` is set (as xfce4-session refuses an answer to a query
    /// called off first), which clears it. A call that names another
    /// object, interface or method goes unanswered.
    fn serve_session_manager(
        manager: &Connection,
        api: SessionApi,
        client: OwnedObjectPath,
        registered: mpsc::Sender<(String, String)>,
        (steps, refuse): (Steps, Arc<AtomicBool>),
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
                        let refused = refuse.swap(false, Ordering::SeqCst);
                        // "answered true" for the yes without a reason the
                        // client gives.
                        let verb = if refused { "refused" } else { "answered" };
                        let step = [format!("{verb} {is_ok}"), reason].concat();
                        steps.lock().unwrap().push(step);
                        if refused {
                            manager
                                .reply_error(
                                    &header,
                                    "org.xfce.Session.Client.Error.InvalidState",
                                    &("Invalid time to respond",),
                                )
                                .unwrap();
                        } else {
                            manager.reply(&header, &()).unwrap();
                        }
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
        /// Refuses the client's next answer (`serve_session_manager`).
        refuse: Arc<AtomicBool>,
        answers: mpsc::Receiver<()>,
        /// The ids of each registration after the first.
        registrations: mpsc::Receiver<(String, String)>,
        result: mpsc::Receiver<zbus::Result<()>>,
    }

    impl Session {
        /// On X11 (`wayland` false) or Wayland.
        fn follow(daemon: &Daemon, api: SessionApi, wayland: bool) -> Self {
            let steps = Steps::default();
            let refuse = Arc::new(AtomicBool::new(false));
            let (registered, registrations) = mpsc::channel();
            let (answered, answers) = mpsc::channel();
            let client = format!("{}/Client1", api.path);
            let manager = daemon.builder().name(api.name).unwrap().build().unwrap();
            serve_session_manager(
                &manager,
                api,
                OwnedObjectPath::try_from(client.as_str()).unwrap(),
                registered,
                (steps.clone(), refuse.clone()),
                answered,
            );
            let (connection, on_end) = (daemon.connect(), noting(&steps));
            let client_name = connection.unique_name().unwrap().to_string();
            let result =
                spawn(move || follow_session_end(&connection, "a-startup-id", wayland, &on_end));
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
                refuse,
                answers,
                registrations,
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
            self.query_answered("answered true");
        }

        /// `query`, with the manager refusing the answer.
        fn query_refused(&self) {
            self.refuse.store(true, Ordering::SeqCst);
            self.query_answered("refused true");
        }

        /// `query`, expecting `answer` noted.
        fn query_answered(&self, answer: &str) {
            let before = self.steps().len();
            self.emit(&self.client, "QueryEndSession");
            self.answers
                .recv_timeout(WAIT)
                .expect("the query was answered");
            assert_eq!(self.steps()[before..].to_vec(), [answer]);
        }

        /// The manager sends `member`, which the client lets pass: it
        /// neither saves nor quits.
        fn passes(&self, member: &str) {
            let before = self.steps();
            self.emit(&self.client, member);
            // Longer than a save, so a client that acted on it would be
            // seen.
            std::thread::sleep(SAVE * 2);
            assert_eq!(self.steps(), before);
            assert!(self.result.try_recv().is_err(), "the client ended");
        }

        /// The manager's `StateChanged` from `old` to `new`, xfce4-session's
        /// client states.
        fn state_changed(&self, old: u32, new: u32) {
            self.manager
                .emit_signal(
                    None::<&str>,
                    self.client.as_str(),
                    self.api.client,
                    "StateChanged",
                    &(old, new),
                )
                .unwrap();
        }

        /// Drops the client as xfce4-session does when a checkpoint or a
        /// query's answer waited out its minute: `StateChanged` from saving
        /// (3) to disconnected, then `Stop`. The client neither saves nor
        /// quits, and registers again with the same ids.
        fn drop_client(&self) {
            self.state_changed(3, XFCE_CLIENT_DISCONNECTED);
            self.passes("Stop");
            assert_eq!(
                self.registrations
                    .recv_timeout(WAIT)
                    .expect("registered again"),
                (APP_ID.to_owned(), "a-startup-id".to_owned())
            );
        }

        /// Calls the logout off: gnome-session sends `CancelEndSession`,
        /// and xfce4-session sends the client nothing (it is set back to
        /// idle). The client neither saves nor quits.
        fn cancel(&self) {
            if self.api.name == GNOME_SESSION.name {
                self.passes("CancelEndSession");
            }
        }

        /// Asks the client whether the session may end, where the query
        /// is the end: the client saves, answers and then waits for the
        /// end, in that order.
        fn query_ends(&self) {
            self.emit(&self.client, "QueryEndSession");
            self.answers
                .recv_timeout(WAIT)
                .expect("the query was answered");
            reach(&self.steps, &["saved", "answered true", "awaits the end"]);
            assert!(self.result.try_recv().is_err(), "the client ended");
        }

        /// The manager sends `member`, and the client ends with `steps`
        /// noted after what it had noted before.
        fn ends_on(&self, member: &str, steps: &[&str]) {
            let before = self.steps().len();
            self.emit(&self.client, member);
            self.result
                .recv_timeout(WAIT)
                .expect("the client ended")
                .unwrap();
            assert_eq!(self.steps()[before..].to_vec(), steps);
        }

        /// Ends the session: the client saves, answers and quits, in that
        /// order, having saved nothing before.
        fn end(&self) {
            let before = self.steps();
            assert!(!before.contains(&"saved".to_owned()), "{before:?}");
            self.ends_on("EndSession", &["saved", "answered true", "quit"]);
        }
    }

    /// The client registers with the app's id and its startup id, ignores
    /// another client's signals, and saves, answers and quits, in that
    /// order. Where the query is not the end, it answers the query at once
    /// without saving, records on through a logout called off, and saves
    /// at the end; where it is (Xfce on Wayland), it saves at the query,
    /// answers and waits for the end, which it answers without a second
    /// save before it quits.
    fn a_logout_saves_before_the_session_manager_is_answered(api: SessionApi, wayland: bool) {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let session = Session::follow(&daemon, api, wayland);
        session.emit(&format!("{}/Client2", api.path), "EndSession");
        if api.query_ends_on_wayland && wayland {
            session.query_ends();
            session.query();
            session.ends_on("EndSession", &["answered true", "quit"]);
        } else {
            session.query();
            session.cancel();
            session.query();
            session.end();
        }
    }

    #[test]
    fn a_gnome_logout_saves_at_the_end() {
        a_logout_saves_before_the_session_manager_is_answered(GNOME_SESSION, false);
        a_logout_saves_before_the_session_manager_is_answered(GNOME_SESSION, true);
    }

    #[test]
    fn an_xfce_logout_on_x11_saves_at_the_end() {
        a_logout_saves_before_the_session_manager_is_answered(XFCE4_SESSION, false);
    }

    #[test]
    fn an_xfce_logout_on_wayland_saves_at_the_query() {
        a_logout_saves_before_the_session_manager_is_answered(XFCE4_SESSION, true);
    }

    /// `Stop`, as xfce4-session sends it for Quit Program before it kills
    /// the process, saves and quits, in that order; after the save at the
    /// query it only quits.
    #[test]
    fn a_stop_saves_and_quits() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let session = Session::follow(&daemon, XFCE4_SESSION, false);
        session.ends_on("Stop", &["saved", "quit"]);
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let session = Session::follow(&daemon, XFCE4_SESSION, true);
        session.query_ends();
        session.ends_on("Stop", &["quit"]);
    }

    /// A client xfce4-session dropped (a checkpoint's save timeout) records
    /// on through the `Stop` that follows and registers again, and so the
    /// next drop, a logout and a Quit Program still reach it.
    #[test]
    fn a_dropped_client_records_on_and_registers_again() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let session = Session::follow(&daemon, XFCE4_SESSION, false);
        session.drop_client();
        session.drop_client();
        session.query();
        session.end();
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let session = Session::follow(&daemon, XFCE4_SESSION, false);
        session.drop_client();
        session.ends_on("Stop", &["saved", "quit"]);
    }

    /// After a query and a logout called off (xfce4-session sets the client
    /// back to idle, `StateChanged` to 0), a checkpoint's drop leaves the
    /// client recording, and a `Stop` alone saves and quits.
    #[test]
    fn a_stop_after_a_logout_called_off_saves_and_quits() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let session = Session::follow(&daemon, XFCE4_SESSION, false);
        session.query();
        session.state_changed(2, 0);
        session.drop_client();
        session.query();
        session.state_changed(2, 0);
        session.ends_on("Stop", &["saved", "quit"]);
    }

    /// A refused answer, as xfce4-session gives one to a query called off
    /// before the answer came, keeps the client following: the drop at
    /// that query's save timeout lets it record on, and the next query and
    /// the end save and quit.
    #[test]
    fn a_refused_answer_keeps_the_client_recording() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let session = Session::follow(&daemon, XFCE4_SESSION, false);
        session.query_refused();
        session.drop_client();
        session.query_refused();
        session.end();
    }

    /// A peer other than the session manager that sends the client's
    /// signals, to every client or straight to the app, is ignored.
    #[test]
    fn a_session_signal_from_another_peer_is_ignored() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let session = Session::follow(&daemon, GNOME_SESSION, false);
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
            (Steps::default(), Arc::default()),
            xfce_answered,
        );
        let session = Session::follow(&daemon, GNOME_SESSION, false);
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
        let session = Session::follow(&daemon, GNOME_SESSION, false);
        // Answered, so the client follows the signals and the bus ends
        // under that wait.
        session.query();
        daemon.end();
        let ended = session.result.recv_timeout(WAIT).expect("the client ended");
        assert!(ended.is_err(), "{ended:?}");
        assert_eq!(session.steps(), ["answered true"]);
    }

    /// No session manager, portal or logind on the bus: both clients end
    /// with an error, and neither saves nor quits.
    #[test]
    fn without_the_services_neither_client_saves_nor_quits() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let on_end = SaveAndQuit {
            save: Arc::new(|| panic!("saved")),
            quit: Arc::new(|| panic!("quit")),
            await_end: Arc::new(|| panic!("awaits the end")),
        };
        assert!(follow_session_end(&daemon.connect(), "", false, &on_end).is_err());
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
    /// query in `steps` ("answered <session>", or "refused <session>" for
    /// the next one once `refuse` is set, which clears it), and serves
    /// each inhibitor's request until it is closed. The states and the
    /// responses are the test's to send (`state`).
    struct FakePortal {
        calls: mpsc::Sender<PortalCall>,
        steps: Steps,
        refuse: Arc<AtomicBool>,
    }

    impl FakePortal {
        fn serve(
            daemon: &Daemon,
            steps: &Steps,
            refuse: &Arc<AtomicBool>,
        ) -> (Connection, mpsc::Receiver<PortalCall>) {
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
                        refuse: refuse.clone(),
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

        fn query_end_response(&self, session_handle: OwnedObjectPath) -> zbus::fdo::Result<()> {
            let refused = self.refuse.swap(false, Ordering::SeqCst);
            let verb = if refused { "refused" } else { "answered" };
            let step = format!("{verb} {}", session_handle.as_str());
            self.steps.lock().unwrap().push(step);
            if refused {
                return Err(zbus::fdo::Error::AccessDenied("not now".to_owned()));
            }
            Ok(())
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
        /// Refuses the client's next answer (`FakePortal`).
        refuse: Arc<AtomicBool>,
        result: mpsc::Receiver<zbus::Result<()>>,
    }

    impl Monitor {
        fn follow(daemon: &Daemon, response: u32) -> Self {
            let (steps, refuse) = (Steps::default(), Arc::default());
            let (portal, calls) = FakePortal::serve(daemon, &steps, &refuse);
            let (connection, on_end) = (daemon.connect(), noting(&steps));
            let client = connection.unique_name().unwrap().to_string();
            let result = spawn(move || follow_session_end(&connection, "", false, &on_end));
            let asked = calls
                .recv_timeout(WAIT)
                .unwrap_or_else(|_| panic!("no monitor was asked for: {:?}", result.try_recv()));
            assert_eq!(
                asked,
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
                refuse,
                result,
            }
        }

        fn steps(&self) -> Vec<String> {
            self.steps.lock().unwrap().clone()
        }

        /// Nothing happened beyond `steps`, for longer than a save takes.
        fn waits(&self, steps: &[&str]) {
            std::thread::sleep(SAVE * 2);
            assert_eq!(self.steps(), steps);
            assert!(self.result.try_recv().is_err(), "the client ended");
        }

        fn ended(&self) -> zbus::Result<()> {
            self.result.recv_timeout(WAIT).expect("the client ended")
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
        monitor.waits(&[]);
        state(portal, client, session, 2);
        let answered = format!("answered {session}");
        reach(&monitor.steps, &[answered.as_str()]);
        // The logout was called off: the session runs again.
        state(portal, client, session, 1);
        monitor.waits(&[answered.as_str()]);
        state(portal, client, session, 3);
        monitor.ended().unwrap();
        assert_eq!(monitor.steps(), [answered.as_str(), "saved", "quit"]);
    }

    /// A refused answer to the portal's query keeps the client following,
    /// and the end saves and quits.
    #[test]
    fn a_refused_portal_answer_keeps_the_client_recording() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let monitor = Monitor::follow(&daemon, 0);
        let (portal, client, session) = (&monitor.portal, &monitor.client, &monitor.session);
        monitor.refuse.store(true, Ordering::SeqCst);
        state(portal, client, session, 2);
        let refused = format!("refused {session}");
        reach(&monitor.steps, &[refused.as_str()]);
        monitor.waits(&[refused.as_str()]);
        state(portal, client, session, 3);
        monitor.ended().unwrap();
        assert_eq!(monitor.steps(), [refused.as_str(), "saved", "quit"]);
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
        monitor.waits(&[]);
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
        let (_portal, calls) = FakePortal::serve(&daemon, &Steps::default(), &Arc::default());
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
