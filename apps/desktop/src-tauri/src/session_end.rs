//! Linux: a logout and a system shutdown or reboot, which reach the app as
//! no exit request of their own. Two D-Bus clients run the shutdown Quit
//! runs before the app goes (`SaveAndQuit`), each on a thread of its own:
//!
//! - **A logout** on GNOME or Xfce: the app registers as a client of the
//!   session manager on the session bus (`RegisterClient` on GNOME's
//!   `org.gnome.SessionManager`, else Xfce's `org.xfce.SessionManager`,
//!   `SessionApi`), answers `QueryEndSession` at once, and on `EndSession`
//!   saves first and answers `EndSessionResponse` after, then quits.
//!   gnome-session waits about ten seconds for that answer (older releases
//!   ninety), as long as `SHUTDOWN_PATIENCE`, so a save that needs all of
//!   its patience can be cut off when the session ends. A `GtkApplication`
//!   that sets `register-session` does the same; tao's does not.
//! - **A system shutdown or reboot**: the app holds logind's `shutdown`
//!   delay lock (`Inhibit` on the system bus), and on
//!   `PrepareForShutdown(true)` it saves and then releases the lock. logind
//!   waits for the lock at most its `InhibitDelayMaxSec`, five seconds by
//!   default, and then goes ahead; the SIGTERM that follows finds the save
//!   running and waits for it (`exit_on_signals` in `main.rs`).
//!
//! Neither follows sleep or the screen lock: a recording goes on through
//! both, as it does on the Mac. A bus that is missing or refuses, a session
//! manager that is not running, or a lock logind denies leaves the app as
//! it was before: it saves when a signal reaches it. A slow or frozen bus
//! holds only its client's thread, never the launch or an exit.
//!
//! Open: KDE Plasma serves no session-manager client API on D-Bus
//! (Plasma's portal has no session monitor; its session manager speaks
//! XSMP to X11 clients, which GTK 3 dropped), so a logout there saves only
//! when systemd signals the app. logind runs there too, so the shutdown
//! lock works there.
//!
//! Swift: none; `AppKit` sends a logout and a shutdown to
//! `applicationShouldTerminate`, which Quit goes through too.

use std::sync::Arc;
use std::time::Duration;

use zbus::blocking::connection::Builder;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type;
use zbus::names::{BusName, OwnedUniqueName};
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedFd, OwnedObjectPath};

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
}

impl SessionApi {
    const GNOME: Self = Self {
        name: "org.gnome.SessionManager",
        path: "/org/gnome/SessionManager",
        manager: "org.gnome.SessionManager",
        client: "org.gnome.SessionManager.ClientPrivate",
    };
    const XFCE: Self = Self {
        name: "org.xfce.SessionManager",
        path: "/org/xfce/SessionManager",
        manager: "org.xfce.Session.Manager",
        client: "org.xfce.Session.Client",
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
    /// The app's: the pipeline quits first, as for an exit signal, then
    /// the shutdown runs on the calling thread's behalf
    /// (`shut_down_before_exit`), then Quit.
    fn of(app: &tauri::AppHandle) -> Self {
        let (saving, quitting) = (app.clone(), app.clone());
        Self {
            save: Arc::new(move || {
                crate::host::host(&saving).quit_pipeline();
                crate::shut_down_before_exit(&saving);
            }),
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
            "a logout saves only when a signal reaches the app",
            move || follow_session(&patient(Builder::session()?)?, &startup_id, &on_end),
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

/// The step for the session manager's signal `member`.
fn client_step(member: &str) -> ClientStep {
    match member {
        "QueryEndSession" => ClientStep::Answer,
        "EndSession" => ClientStep::SaveAnswerQuit,
        "Stop" => ClientStep::Quit,
        _ => ClientStep::Wait,
    }
}

/// The first session manager of `SessionApi::ALL` on `session`, with the
/// unique name that owns it. Asked of the bus (`GetNameOwner`), so asking
/// starts none.
fn session_manager(session: &Connection) -> zbus::Result<(SessionApi, OwnedUniqueName)> {
    let bus = zbus::blocking::fdo::DBusProxy::builder(session)
        .cache_properties(CacheProperties::No)
        .build()?;
    for api in SessionApi::ALL {
        match bus.get_name_owner(BusName::try_from(api.name)?) {
            Ok(owner) => return Ok((api, owner)),
            Err(zbus::fdo::Error::NameHasNoOwner(_)) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(zbus::Error::Failure(
        "no session manager on the bus".to_owned(),
    ))
}

/// Registers the app with the session manager on `session` and follows
/// its signals to this client until the session ends; an error when no
/// manager is there or the bus goes away before the app quit.
fn follow_session(
    session: &Connection,
    startup_id: &str,
    on_end: &SaveAndQuit,
) -> zbus::Result<()> {
    let (api, owner) = session_manager(session)?;
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
        match client_step(member.as_str()) {
            // A lost answer leaves the client following: the end may still
            // come.
            ClientStep::Answer => {
                if let Err(error) = answer() {
                    tracing::warn!(%error, "the session manager's query went unanswered");
                }
            }
            ClientStep::SaveAnswerQuit => {
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
            (on_end.save)();
            drop(lock);
            (on_end.quit)();
            return Ok(());
        }
    }
    Err(zbus::Error::Failure("the system bus closed".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, Read as _};
    use std::sync::{Mutex, mpsc};

    #[test]
    fn the_session_managers_signals_map_to_their_steps() {
        assert_eq!(client_step("QueryEndSession"), ClientStep::Answer);
        assert_eq!(client_step("EndSession"), ClientStep::SaveAnswerQuit);
        assert_eq!(client_step("Stop"), ClientStep::Quit);
        assert_eq!(client_step("CancelEndSession"), ClientStep::Wait);
        assert_eq!(client_step("Unknown"), ClientStep::Wait);
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

    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
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
        let (inhibited, inhibits) = mpsc::channel();
        let logind = daemon
            .builder()
            .name(LOGIND)
            .unwrap()
            .serve_at(LOGIND_PATH, FakeLogind { inhibited })
            .unwrap()
            .build()
            .unwrap();
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

    /// The names gnome-session and xfce4-session serve, written out apart
    /// from `SessionApi`'s so that a wrong name there fails the tests.
    const GNOME_SESSION: SessionApi = SessionApi {
        name: "org.gnome.SessionManager",
        path: "/org/gnome/SessionManager",
        manager: "org.gnome.SessionManager",
        client: "org.gnome.SessionManager.ClientPrivate",
    };
    const XFCE4_SESSION: SessionApi = SessionApi {
        name: "org.xfce.SessionManager",
        path: "/org/xfce/SessionManager",
        manager: "org.xfce.Session.Manager",
        client: "org.xfce.Session.Client",
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
            let result = spawn(move || follow_session(&connection, "a-startup-id", &on_end));
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

    /// The client registers with the app's id and its startup id, answers
    /// a query at once without saving, ignores another client's signals,
    /// and on the session's end saves, answers and quits, in that order.
    fn a_logout_saves_before_the_session_manager_is_answered(api: SessionApi) {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let session = Session::follow(&daemon, api);
        session.emit(&format!("{}/Client2", api.path), "EndSession");
        session.query();
        session.end();
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
        assert!(follow_session(&daemon.connect(), "", &on_end).is_err());
        assert!(hold_shutdown_lock(&daemon.connect(), &on_end).is_err());
    }
}
