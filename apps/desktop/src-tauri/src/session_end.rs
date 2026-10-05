//! Linux: a logout and a shutdown, which reach the app as no exit request
//! of their own. Two D-Bus clients run the shutdown Quit runs before the
//! app goes (`Ending`), each on a thread of its own:
//!
//! - **A logout** on GNOME, and on every desktop whose session manager
//!   serves GNOME's client API (`org.gnome.SessionManager` on the session
//!   bus): the app registers as a session client (`RegisterClient`),
//!   answers `QueryEndSession` at once, and on `EndSession` saves first and
//!   answers `EndSessionResponse` after, so the session ends, and the
//!   display closes, only once the recording is saved (the session manager
//!   waits a bounded time for every client's answer). A `GtkApplication`
//!   that sets `register-session` does the same; tao's does not.
//! - **A shutdown or a reboot**: the app holds logind's `shutdown` delay
//!   lock (`Inhibit` on the system bus), and on `PrepareForShutdown(true)`
//!   it saves and then releases the lock. logind waits for the lock at most
//!   its `InhibitDelayMaxSec`, five seconds by default, and then goes
//!   ahead; the SIGTERM that follows finds the save running and waits for
//!   it (`exit_on_signals` in `main.rs`).
//!
//! Neither follows sleep or the screen lock: a recording goes on through
//! both, as it does on the Mac. A bus that is missing or refuses, a session
//! manager that is not running, or a lock logind denies leaves the app as
//! it was before: it saves when a signal reaches it. A slow or frozen bus
//! holds only its client's thread, never the launch or an exit.
//!
//! Open: KDE Plasma and Xfce serve neither API (Plasma's portal has no
//! session monitor; both speak XSMP to X11 clients, which GTK 3 dropped),
//! so a logout there saves only when systemd signals the app.
//!
//! Swift: none; `AppKit` sends a logout and a shutdown to
//! `applicationShouldTerminate`, which Quit goes through too.

use std::sync::Arc;
use std::time::Duration;

use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type;
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedFd, OwnedObjectPath};

/// GNOME's session manager, its object and the interface a client
/// registers through.
const SESSION_MANAGER: &str = "org.gnome.SessionManager";
const SESSION_MANAGER_PATH: &str = "/org/gnome/SessionManager";
/// The interface of a registered client's object: the manager's signals to
/// it and its answer.
const CLIENT_PRIVATE: &str = "org.gnome.SessionManager.ClientPrivate";
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
pub struct Ending {
    save: Arc<dyn Fn() + Send + Sync>,
    quit: Arc<dyn Fn() + Send + Sync>,
}

impl Ending {
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

/// Starts both clients, the session's only with a session bus (the
/// definition the single instance uses, `session_bus_named`).
pub fn watch(app: &tauri::AppHandle) {
    let ending = Ending::of(app);
    if crate::session_bus_named() {
        let ending = ending.clone();
        run(
            "steno-session-client",
            "a logout saves only when a signal reaches the app",
            move || follow_session(&connect(Bus::Session)?, &startup_id(), &ending),
        );
    }
    run(
        "steno-shutdown-lock",
        "a shutdown saves only when a signal reaches the app",
        move || hold_shutdown_lock(&connect(Bus::System)?, &ending),
    );
}

/// Which bus a client connects to.
#[derive(Debug, Clone, Copy)]
enum Bus {
    Session,
    System,
}

/// A connection to `bus` whose method calls wait `CALL_PATIENCE` at most.
fn connect(bus: Bus) -> zbus::Result<Connection> {
    let builder = match bus {
        Bus::Session => zbus::blocking::connection::Builder::session()?,
        Bus::System => zbus::blocking::connection::Builder::system()?,
    };
    builder.method_timeout(CALL_PATIENCE).build()
}

/// Runs `client` on a thread named `name`; an error ends it with `gap`,
/// what the app is left without, in the log.
fn run(
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

/// The startup id the session manager gave the app when it started it
/// (an autostart at login), so it knows the client; empty otherwise.
fn startup_id() -> String {
    std::env::var("DESKTOP_AUTOSTART_ID").unwrap_or_default()
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

/// Registers the app with the session manager on `session` and follows
/// its signals to this client until the session ends; an error when the
/// manager is not there or the bus goes away.
fn follow_session(session: &Connection, startup_id: &str, ending: &Ending) -> zbus::Result<()> {
    // Subscribed before the app registers, so no signal falls between.
    let rule = zbus::MatchRule::builder()
        .msg_type(Type::Signal)
        .sender(SESSION_MANAGER)?
        .interface(CLIENT_PRIVATE)?
        .build();
    let signals = MessageIterator::for_match_rule(rule, session, None)?;
    let manager = proxy(
        session,
        SESSION_MANAGER,
        SESSION_MANAGER_PATH,
        SESSION_MANAGER,
    )?;
    let client: OwnedObjectPath = manager.call("RegisterClient", &(APP_ID, startup_id))?;
    let answer = || {
        session
            .call_method(
                Some(SESSION_MANAGER),
                client.as_str(),
                Some(CLIENT_PRIVATE),
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
                (ending.save)();
                let answered = answer();
                (ending.quit)();
                return answered;
            }
            ClientStep::Quit => {
                (ending.quit)();
                return Ok(());
            }
            ClientStep::Wait => {}
        }
    }
    Ok(())
}

/// Holds logind's `shutdown` delay lock on `system` until a shutdown
/// begins, then saves, releases it and quits; an error when logind is not
/// there or denies the lock.
fn hold_shutdown_lock(system: &Connection, ending: &Ending) -> zbus::Result<()> {
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
            (ending.save)();
            drop(lock);
            (ending.quit)();
            return Ok(());
        }
    }
    Ok(())
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

        fn connect(&self) -> Connection {
            zbus::blocking::connection::Builder::address(self.address.as_str())
                .unwrap()
                .method_timeout(CALL_PATIENCE)
                .build()
                .unwrap()
        }
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// An `Ending` that notes its steps; `save` takes a moment, so a lock
    /// released or an answer sent before it ended would be noted first.
    fn noting(steps: &Arc<Mutex<Vec<String>>>) -> Ending {
        let (saving, quitting) = (steps.clone(), steps.clone());
        Ending {
            save: Arc::new(move || {
                std::thread::sleep(Duration::from_millis(200));
                saving.lock().unwrap().push("saved".to_owned());
            }),
            quit: Arc::new(move || quitting.lock().unwrap().push("quit".to_owned())),
        }
    }

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

    /// The client takes the `shutdown` delay lock, ignores a shutdown
    /// called off, and on a shutdown saves, releases the lock after the
    /// save, and quits.
    #[test]
    fn a_shutdown_saves_before_the_lock_is_released_and_then_quits() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let (inhibited, inhibits) = mpsc::channel();
        let logind = zbus::blocking::connection::Builder::address(daemon.address.as_str())
            .unwrap()
            .name(LOGIND)
            .unwrap()
            .serve_at(LOGIND_PATH, FakeLogind { inhibited })
            .unwrap()
            .build()
            .unwrap();
        let steps = Arc::new(Mutex::new(Vec::new()));
        let (client, ending) = (daemon.connect(), noting(&steps));
        let result = spawn(move || hold_shutdown_lock(&client, &ending));

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

    /// GNOME's session manager as far as one client goes: it registers it
    /// at `CLIENT` and notes its answers.
    struct FakeSessionManager {
        registered: mpsc::Sender<(String, String)>,
    }

    const CLIENT: &str = "/org/gnome/SessionManager/Client1";

    #[zbus::interface(name = "org.gnome.SessionManager")]
    impl FakeSessionManager {
        fn register_client(&self, app_id: String, startup_id: String) -> OwnedObjectPath {
            let _ = self.registered.send((app_id, startup_id));
            OwnedObjectPath::try_from(CLIENT).unwrap()
        }
    }

    struct FakeClient {
        steps: Arc<Mutex<Vec<String>>>,
        answered: mpsc::Sender<()>,
    }

    #[zbus::interface(name = "org.gnome.SessionManager.ClientPrivate")]
    impl FakeClient {
        fn end_session_response(&self, is_ok: bool, reason: String) {
            // "answered true" for the yes without a reason the client gives.
            let step = [format!("answered {is_ok}"), reason].concat();
            self.steps.lock().unwrap().push(step);
            let _ = self.answered.send(());
        }
    }

    /// The client registers with the app's id and its startup id, answers
    /// a query at once without saving, ignores another client's signals,
    /// and on the session's end saves, answers and quits, in that order.
    #[test]
    fn a_logout_saves_before_the_session_manager_is_answered() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let steps = Arc::new(Mutex::new(Vec::new()));
        let (registered, registrations) = mpsc::channel();
        let (answered, answers) = mpsc::channel();
        let manager = zbus::blocking::connection::Builder::address(daemon.address.as_str())
            .unwrap()
            .name(SESSION_MANAGER)
            .unwrap()
            .serve_at(SESSION_MANAGER_PATH, FakeSessionManager { registered })
            .unwrap()
            .serve_at(
                CLIENT,
                FakeClient {
                    steps: steps.clone(),
                    answered,
                },
            )
            .unwrap()
            .build()
            .unwrap();
        let (client, ending) = (daemon.connect(), noting(&steps));
        let result = spawn(move || follow_session(&client, "a-startup-id", &ending));

        assert_eq!(
            registrations.recv_timeout(WAIT).expect("registered"),
            (APP_ID.to_owned(), "a-startup-id".to_owned())
        );
        let emit = |path: &str, member: &str| {
            manager
                .emit_signal(None::<&str>, path, CLIENT_PRIVATE, member, &(0_u32,))
                .unwrap();
        };
        emit("/org/gnome/SessionManager/Client2", "EndSession");
        emit(CLIENT, "QueryEndSession");
        answers.recv_timeout(WAIT).expect("the query was answered");
        assert_eq!(*steps.lock().unwrap(), ["answered true"]);
        emit(CLIENT, "EndSession");
        result
            .recv_timeout(WAIT)
            .expect("the client ended")
            .unwrap();
        assert_eq!(
            *steps.lock().unwrap(),
            ["answered true", "saved", "answered true", "quit"]
        );
    }

    /// No session manager and no logind on the bus: both clients end with
    /// an error, and neither saves nor quits.
    #[test]
    fn without_the_services_neither_client_saves_or_quits() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let ending = Ending {
            save: Arc::new(|| panic!("saved")),
            quit: Arc::new(|| panic!("quit")),
        };
        assert!(follow_session(&daemon.connect(), "", &ending).is_err());
        assert!(hold_shutdown_lock(&daemon.connect(), &ending).is_err());
    }
}
