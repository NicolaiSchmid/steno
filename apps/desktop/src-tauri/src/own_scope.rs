//! Linux only: the app in a systemd scope of its own when it starts inside
//! another program's service (the compositor's under uwsm, whose stop would
//! end Steno unsaved), and each `AppImage` mount server in a scope of its own
//! so a unit's stop cannot end the mount while the app saves. A queued start
//! is never called off, and nothing here stops a scope.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// How long the launch waits for the move before it goes on.
const MOVE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the request's thread waits for a start that is still queued:
/// twice uwsm's `TimeoutStartSec=30`, which bounds reaching
/// `graphical-session.target`.
const QUEUE_LIMIT: Duration = Duration::from_secs(60);

/// How long each call to the user manager may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(2);

const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The scope's stop timeout, as `stop_timeout` gives the drop-ins.
const STOP_TIMEOUT: Duration = Duration::from_secs(20);

// systemd's manager: its bus name, object path and interfaces.
const SYSTEMD: &str = "org.freedesktop.systemd1";
const SYSTEMD_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MANAGER: &str = "org.freedesktop.systemd1.Manager";
const SYSTEMD_JOB: &str = "org.freedesktop.systemd1.Job";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// Why a process is not in the scope it was to move into.
#[derive(Debug, thiserror::Error)]
enum MoveError {
    #[error("no user bus: XDG_RUNTIME_DIR is not an absolute path")]
    NoRuntimeDir,
    #[error("no user bus: {0}")]
    NoBus(std::io::Error),
    #[error(transparent)]
    Bus(#[from] zbus::Error),
    #[error("the user manager's job ended without the move")]
    NotMoved,
    #[error(
        "the user manager had not started the scope {} s after the launch; its start stays queued",
        QUEUE_LIMIT.as_secs()
    )]
    StillQueued,
    #[error("the move still waits for the user manager")]
    Waiting,
    #[error("the request's thread did not start: {0}")]
    Thread(std::io::Error),
}

/// The scope the app moved into, `None` when it stays as its service's
/// main process, or why it did not move.
type Outcome = Result<Option<String>, MoveError>;

/// The cgroup path of a cgroup file's unified (`0::`) line.
fn unified(cgroup: &str) -> Option<&str> {
    cgroup.lines().find_map(|line| line.strip_prefix("0::"))
}

fn innermost(cgroup: &str) -> Option<&str> {
    unified(cgroup)?.rsplit('/').next()
}

/// The service of another program the app runs in: a user manager service
/// whose name does not say `steno`. `None` for a scope, Steno's own
/// service, a sub-cgroup, a path outside a user manager, and cgroup v1.
fn foreign_service(cgroup: &str) -> Option<&str> {
    let unit = user_unit(cgroup, ".service")?;
    (!unit.to_ascii_lowercase().contains("steno")).then_some(unit)
}

/// The user manager unit a cgroup file names, when its name ends in
/// `suffix` and is a unit's (not a sub-cgroup, not escaped).
fn user_unit<'a>(cgroup: &'a str, suffix: &str) -> Option<&'a str> {
    let mut parts = unified(cgroup)?.rsplit('/');
    let unit = parts.next()?;
    let in_user_manager = parts.any(|part| part.starts_with("user@") && part.ends_with(".service"));
    let named = unit
        .strip_suffix(suffix)
        .is_some_and(|stem| !stem.is_empty() && !stem.starts_with('_'));
    (in_user_manager && named).then_some(unit)
}

/// The app's own unit in the user manager, which the mount servers' scopes
/// are ordered against when the app stays.
fn own_unit(cgroup: &str) -> Option<&str> {
    user_unit(cgroup, ".scope").or_else(|| user_unit(cgroup, ".service"))
}

/// Whether the process is in a login's `session-<n>.scope`.
fn in_login_session(cgroup: &str) -> bool {
    unified(cgroup).is_some_and(|path| {
        path.starts_with("/user.slice/")
            && innermost(cgroup)
                .and_then(|unit| unit.strip_prefix("session-")?.strip_suffix(".scope"))
                .is_some_and(|n| !n.is_empty())
    })
}

/// What the launch moves.
#[derive(Debug, PartialEq, Eq)]
enum Plan<'a> {
    /// The app leaves another program's service, the mount servers with it.
    Leave(&'a str, Vec<u32>),
    /// The app stays in its own unit; the mount servers leave theirs.
    MoveServers(Vec<u32>, &'a str),
    /// The app is in a login's session scope; the mount servers stay.
    ServersStay(Vec<u32>),
    /// Nothing moves.
    Stay,
}

/// What the launch moves, from the app's cgroup file and the image's mount
/// servers.
fn plan(cgroup: &str, servers: Vec<u32>) -> Plan<'_> {
    if let Some(unit) = foreign_service(cgroup) {
        return Plan::Leave(unit, servers);
    }
    if servers.is_empty() {
        return Plan::Stay;
    }
    match own_unit(cgroup) {
        Some(unit) => Plan::MoveServers(servers, unit),
        None if in_login_session(cgroup) => Plan::ServersStay(servers),
        None => Plan::Stay,
    }
}

/// The mount servers not in a mount server's scope yet; one that is (an
/// earlier run's) would fail its start as a unit that exists.
fn unmoved(servers: Vec<u32>, cgroup_of: CgroupOf) -> Vec<u32> {
    servers
        .into_iter()
        .filter(|&server| {
            !cgroup_of(server).is_ok_and(|cgroup| {
                innermost(&cgroup).is_some_and(|unit| {
                    unit.strip_prefix(SERVER_SCOPE)
                        .and_then(|rest| rest.strip_suffix(".scope"))
                        .is_some_and(|pid| !pid.is_empty())
                })
            })
        })
        .collect()
}

/// The app's scope, named per the XDG convention with the pid as the
/// random part.
fn scope_name(pid: u32) -> String {
    format!("app-steno\\x2ddesktop-{pid}.scope")
}

/// What a mount server's scope's name starts with, its pid after it.
const SERVER_SCOPE: &str = "app-steno\\x2ddesktop\\x2dimage-";

fn server_scope_name(server: u32) -> String {
    format!("{SERVER_SCOPE}{server}.scope")
}

/// What the app's scope is started with.
fn properties(pid: u32) -> Vec<(&'static str, Value<'static>)> {
    let target = || Value::from(vec!["graphical-session.target"]);
    let micros = u64::try_from(STOP_TIMEOUT.as_micros()).expect("20 s fits");
    vec![
        ("Description", Value::from("Steno")),
        ("PIDs", Value::from(vec![pid])),
        ("Slice", Value::from("app-graphical.slice")),
        ("TimeoutStopUSec", Value::from(micros)),
        ("PartOf", target()),
        ("After", target()),
        // A killed app's scope ends failed; collect it anyway.
        ("CollectMode", Value::from("inactive-or-failed")),
    ]
}

/// What a mount server's scope is started with: outside the graphical
/// slices, stopped after the app's unit `app`.
fn server_properties(server: u32, app: &str) -> Vec<(&'static str, Value<'static>)> {
    vec![
        ("Description", Value::from("Steno's AppImage mount")),
        ("PIDs", Value::from(vec![server])),
        ("Slice", Value::from("app.slice")),
        ("Before", Value::from(vec![app.to_owned()])),
        ("CollectMode", Value::from("inactive-or-failed")),
    ]
}

fn call<B, R>(
    connection: &Connection,
    path: &str,
    interface: &str,
    method: &str,
    body: &B,
) -> zbus::Result<R>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
    R: serde::de::DeserializeOwned + zbus::zvariant::Type,
{
    connection
        .call_method(Some(SYSTEMD), path, Some(interface), method, body)?
        .body()
        .deserialize()
}

fn main_pid(connection: &Connection, unit: &str) -> zbus::Result<u32> {
    let path: OwnedObjectPath = call(
        connection,
        SYSTEMD_PATH,
        SYSTEMD_MANAGER,
        "GetUnit",
        &(unit,),
    )?;
    let pid: OwnedValue = call(
        connection,
        path.as_str(),
        PROPERTIES,
        "Get",
        &("org.freedesktop.systemd1.Service", "MainPID"),
    )?;
    Ok(u32::try_from(pid)?)
}

/// Starts the transient scope `name`; the job it queued.
fn start_scope(
    connection: &Connection,
    name: &str,
    properties: Vec<(&str, Value<'_>)>,
) -> zbus::Result<OwnedObjectPath> {
    let auxiliary: Vec<(&str, Vec<(&str, Value)>)> = Vec::new();
    call(
        connection,
        SYSTEMD_PATH,
        SYSTEMD_MANAGER,
        "StartTransientUnit",
        &(name, "fail", properties, auxiliary),
    )
}

/// Moves the mount server `server` into a scope of its own, stopped after
/// `app`, waiting until it is in, the job has ended, or `deadline` passes.
fn move_server(
    connection: &Connection,
    server: u32,
    app: &str,
    cgroup_of: CgroupOf,
    deadline: Instant,
) -> Result<String, MoveError> {
    let scope = server_scope_name(server);
    let job = start_scope(connection, &scope, server_properties(server, app))?;
    joined(
        connection,
        &job,
        || in_unit(cgroup_of, server, &scope),
        deadline,
    )?;
    Ok(scope)
}

/// [`move_server`] for each mount server against the one `deadline`, each
/// move written to stderr and each failure logged.
fn take_servers(
    connection: &Connection,
    servers: &[u32],
    app: &str,
    cgroup_of: CgroupOf,
    deadline: Instant,
) {
    for &server in servers {
        match move_server(connection, server, app, cgroup_of, deadline) {
            Ok(scope) => stderr_line!(
                "[steno-desktop] moved the AppImage's mount server {server} into a scope of its own, {scope}, which the session's end does not stop"
            ),
            Err(error) => server_stays(server, &error),
        }
    }
}

fn server_stays(server: u32, error: &dyn std::fmt::Display) {
    tracing::warn!(
        server,
        %error,
        "the AppImage's mount server stays where it runs, and the stop of that unit at the session's end may end it while Steno saves a recording"
    );
}

fn servers_stay(servers: &[u32], error: &dyn std::fmt::Display) {
    for &server in servers {
        server_stays(server, error);
    }
}

fn job_ended(connection: &Connection, job: &OwnedObjectPath) -> zbus::Result<bool> {
    match call::<_, OwnedValue>(
        connection,
        job.as_str(),
        PROPERTIES,
        "Get",
        &(SYSTEMD_JOB, "State"),
    ) {
        Ok(_) => Ok(false),
        Err(zbus::Error::MethodError(..)) => Ok(true),
        Err(error) => Err(error),
    }
}

fn in_unit(cgroup_of: CgroupOf, pid: u32, name: &str) -> bool {
    cgroup_of(pid).is_ok_and(|cgroup| innermost(&cgroup) == Some(name))
}

/// Waits for the start `job` to move a process: `NotMoved` once the job
/// ended without it, `StillQueued` past `deadline`. Nothing is called off.
fn joined(
    connection: &Connection,
    job: &OwnedObjectPath,
    moved: impl Fn() -> bool,
    deadline: Instant,
) -> Result<(), MoveError> {
    loop {
        if moved() {
            return Ok(());
        }
        if job_ended(connection, job)? {
            return if moved() {
                Ok(())
            } else {
                Err(MoveError::NotMoved)
            };
        }
        if Instant::now() >= deadline {
            return Err(MoveError::StillQueued);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Moves the app out of another program's service `unit` into a scope of
/// its own, and each mount server into one beside it; `None` when the app
/// is `unit`'s main process and stays.
///
/// The app's scope starts first so the mount servers' can be ordered before
/// it. `StartTransientUnit` answers once the job is queued; at login it
/// moves the app only once `graphical-session.target` is reached, so a job
/// still queued at `deadline` stays queued.
fn move_out(
    connection: &Connection,
    unit: &str,
    pid: u32,
    servers: &[u32],
    cgroup_of: CgroupOf,
    deadline: Instant,
) -> Outcome {
    let name = scope_name(pid);
    let started = match main_pid(connection, unit) {
        Ok(main) if main == pid => Ok(None),
        Ok(_) => start_scope(connection, &name, properties(pid)).map(Some),
        Err(error) => Err(error),
    };
    let before = if matches!(started, Ok(Some(_))) {
        name.as_str()
    } else {
        unit
    };
    take_servers(connection, servers, before, cgroup_of, deadline);
    let Some(job) = started? else {
        return Ok(None);
    };
    joined(
        connection,
        &job,
        || in_unit(cgroup_of, pid, &name),
        deadline,
    )?;
    Ok(Some(name))
}

/// Reads the cgroup file of a process: [`cgroup_of`], or a test's.
type CgroupOf = fn(u32) -> std::io::Result<String>;

fn cgroup_of(pid: u32) -> std::io::Result<String> {
    std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
}

/// A connection to the user bus's socket in `$XDG_RUNTIME_DIR`.
fn user_bus() -> Result<Connection, MoveError> {
    bus_in(
        std::env::var_os("XDG_RUNTIME_DIR")
            .as_deref()
            .map(Path::new),
    )
}

/// A connection to the bus socket in the runtime directory `runtime`
/// (absolute), each call answered within [`CALL_TIMEOUT`] or failed.
fn bus_in(runtime: Option<&Path>) -> Result<Connection, MoveError> {
    let runtime = runtime
        .filter(|dir| dir.is_absolute())
        .ok_or(MoveError::NoRuntimeDir)?;
    let stream = UnixStream::connect(runtime.join("bus")).map_err(MoveError::NoBus)?;
    Ok(Builder::async_io_unix_stream(stream)
        .method_timeout(CALL_TIMEOUT)
        .build()?)
}

/// Runs `f` on a thread named `name` that logs where the caller logs.
fn spawn<T: Send + 'static>(
    name: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<T>> {
    let dispatch = tracing::dispatcher::get_default(Clone::clone);
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || tracing::dispatcher::with_default(&dispatch, f))
}

/// Runs `request` on a thread of its own and waits at most `wait`; after
/// that the thread hands its outcome to `late`. The outcome reaches exactly
/// one of the two.
fn move_within(
    wait: Duration,
    request: impl FnOnce() -> Outcome + Send + 'static,
    late: impl FnOnce(Outcome) + Send + 'static,
) -> Outcome {
    let (done, outcome) = mpsc::sync_channel(0);
    let spawned = spawn("steno-own-scope", move || {
        if let Err(mpsc::SendError(outcome)) = done.send(request()) {
            late(outcome);
        }
    });
    match spawned {
        Ok(_) => outcome
            .recv_timeout(wait)
            .unwrap_or(Err(MoveError::Waiting)),
        Err(error) => Err(MoveError::Thread(error)),
    }
}

fn report(unit: &str, outcome: &Outcome, late: bool) {
    let when = if late {
        ", after the launch went on"
    } else {
        ""
    };
    match outcome {
        Ok(Some(scope)) => stderr_line!(
            "[steno-desktop] started inside {unit} and moved into a scope of its own{when}, {scope}, where a save at the session's end has {} s",
            STOP_TIMEOUT.as_secs()
        ),
        Ok(None) => tracing::debug!(%unit, "Steno is its service's main process and stays in it"),
        Err(MoveError::Waiting) => tracing::info!(
            %unit,
            "the move into a scope of its own still waits for the user manager, at login until graphical-session.target is reached; its outcome follows"
        ),
        Err(error) => {
            let now = cgroup_of(std::process::id()).ok();
            let now = now.as_deref().and_then(innermost).unwrap_or("unknown");
            tracing::warn!(
                %unit,
                %error,
                now,
                late,
                "Steno runs inside another program's service, whose stop may cut off the save of a recording; start Steno from the app launcher (on a uwsm session, with `uwsm-app -- steno-desktop`)"
            );
        }
    }
}

/// Moves the app and the `AppImage`'s mount servers into scopes of their
/// own (see the module docs). Called at launch, before the first window.
/// Never fails.
pub fn take_own_scopes() {
    launch(
        std::process::id(),
        crate::appimage::mount_servers,
        user_bus,
        cgroup_of,
        Instant::now() + QUEUE_LIMIT,
    );
}

/// [`carry_out`] for the app `pid`, its plan made from its cgroup and the
/// unmoved mount servers; nothing when its cgroup cannot be read.
fn launch(
    pid: u32,
    mount_servers: impl FnOnce() -> Vec<u32>,
    connect: impl FnOnce() -> Result<Connection, MoveError> + Send + 'static,
    cgroup_of: CgroupOf,
    deadline: Instant,
) -> Option<std::thread::JoinHandle<()>> {
    let cgroup = cgroup_of(pid).ok()?;
    let servers = unmoved(mount_servers(), cgroup_of);
    carry_out(plan(&cgroup, servers), pid, connect, cgroup_of, deadline)
}

/// Carries out `plan` for the app `pid` until `deadline`. Returns the
/// thread that moves the mount servers of an app that stays.
fn carry_out(
    plan: Plan<'_>,
    pid: u32,
    connect: impl FnOnce() -> Result<Connection, MoveError> + Send + 'static,
    cgroup_of: CgroupOf,
    deadline: Instant,
) -> Option<std::thread::JoinHandle<()>> {
    match plan {
        Plan::Leave(unit, servers) => {
            let (request, late) = (unit.to_owned(), unit.to_owned());
            let outcome = move_within(
                MOVE_TIMEOUT,
                move || {
                    let connection =
                        connect().inspect_err(|error| servers_stay(&servers, error))?;
                    move_out(&connection, &request, pid, &servers, cgroup_of, deadline)
                },
                move |outcome| report(&late, &outcome, true),
            );
            report(unit, &outcome, false);
            None
        }
        Plan::MoveServers(servers, unit) => {
            let app = unit.to_owned();
            let stay = servers.clone();
            spawn("steno-server-scopes", move || match connect() {
                Ok(connection) => take_servers(&connection, &servers, &app, cgroup_of, deadline),
                Err(error) => servers_stay(&servers, &error),
            })
            .inspect_err(|error| servers_stay(&stay, error))
            .ok()
        }
        Plan::ServersStay(servers) => {
            servers_stay(
                &servers,
                &"Steno runs in a login's session scope, outside the user manager",
            );
            None
        }
        Plan::Stay => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use crate::appimage::tests::warnings;
    use crate::dbus::tests::Daemon;

    const USER: &str = "0::/user.slice/user-1000.slice/user@1000.service";
    const HYPRLAND: &str = "wayland-wm@hyprland.desktop.service";
    /// GNOME's scope, which the app 4242 stays in.
    const GNOME: &str = "app-gnome-steno\\x2ddesktop-4242.scope";

    fn below_user_manager(rest: &str) -> String {
        format!("{USER}/{rest}\n")
    }

    #[test]
    fn only_another_programs_service_in_the_user_manager_is_left() {
        assert_eq!(
            foreign_service(&below_user_manager(&format!("session.slice/{HYPRLAND}"))),
            Some(HYPRLAND)
        );
        // A launcher's service; a hybrid system's v1 lines are skipped.
        assert_eq!(
            foreign_service(&below_user_manager(
                "app.slice/app-launcher@autostart.service"
            )),
            Some("app-launcher@autostart.service")
        );
        assert_eq!(
            foreign_service(&format!(
                "1:name=systemd:/x\n{}",
                below_user_manager(&format!("session.slice/{HYPRLAND}"))
            )),
            Some(HYPRLAND)
        );
        for stays in [
            // uwsm-app's scope, GNOME's, a terminal's.
            below_user_manager(
                "app.slice/app-graphical.slice/app-Hyprland-steno\\x2ddesktop-1a2b3c4d.scope",
            ),
            below_user_manager("app.slice/app-gnome-steno\\x2ddesktop-4242.scope"),
            below_user_manager("app.slice/app-Hyprland-kitty-1a2b3c4d.scope"),
            // Steno's own services.
            below_user_manager("app.slice/app-steno\\x2ddesktop@autostart.service"),
            below_user_manager(
                "app.slice/app-graphical.slice/app-Hyprland-steno\\x2ddesktop@1a2b3c4d.service",
            ),
            below_user_manager("app.slice/Steno.service"),
            // A sub-cgroup, an escaped name, no name, the manager itself.
            below_user_manager(&format!("session.slice/{HYPRLAND}/sub")),
            below_user_manager("session.slice/_cgroup.service"),
            below_user_manager("session.slice/.service"),
            format!("{USER}\n"),
            below_user_manager("init.scope"),
            // A system service, an ssh login, a container's root, v1 only.
            "0::/system.slice/display-manager.service".to_owned(),
            "0::/user.slice/user-1000.slice/session-3.scope".to_owned(),
            "0::/".to_owned(),
            format!(
                "1:name=systemd:/user.slice/user-1000.slice/user@1000.service/session.slice/{HYPRLAND}"
            ),
            String::new(),
            "0::/machine.slice/systemd-nspawn@x.service/payload/system.slice/getty.service"
                .to_owned(),
            "0::/user.slice/user@1000.slice/app.service".to_owned(),
        ] {
            assert_eq!(foreign_service(&stays), None, "{stays}");
        }
    }

    #[test]
    fn the_apps_own_unit_is_a_scope_or_service_of_the_user_manager() {
        for (cgroup, unit) in [
            (
                below_user_manager("app.slice/app-gnome-steno\\x2ddesktop-4242.scope"),
                Some("app-gnome-steno\\x2ddesktop-4242.scope"),
            ),
            (
                below_user_manager("app.slice/app-steno\\x2ddesktop@autostart.service"),
                Some("app-steno\\x2ddesktop@autostart.service"),
            ),
            (
                below_user_manager(&format!("session.slice/{HYPRLAND}")),
                Some(HYPRLAND),
            ),
            (below_user_manager("app.slice/x.scope/sub"), None),
            (below_user_manager("app.slice/_x.scope"), None),
            (format!("{USER}\n"), None),
            (
                "0::/user.slice/user-1000.slice/session-3.scope".to_owned(),
                None,
            ),
            ("0::/system.slice/display-manager.service".to_owned(), None),
        ] {
            assert_eq!(own_unit(&cgroup), unit, "{cgroup}");
        }
    }

    #[test]
    fn the_scope_outlasts_the_save_and_ends_with_the_graphical_session() {
        assert_eq!(scope_name(4242), "app-steno\\x2ddesktop-4242.scope");
        assert_eq!(
            server_scope_name(77),
            "app-steno\\x2ddesktop\\x2dimage-77.scope"
        );
        assert_eq!(
            innermost(&below_user_manager("app.slice/x.scope")),
            Some("x.scope")
        );
        let save = steno_services::app::SHUTDOWN_PATIENCE + crate::EXIT_GRACE;
        assert!(STOP_TIMEOUT >= save * 3 / 2);
        assert!(MOVE_TIMEOUT <= Duration::from_secs(3));
        assert!(QUEUE_LIMIT > Duration::from_secs(30));
    }

    /// The launch's choice for each kind of cgroup.
    #[test]
    fn the_plan_moves_the_app_out_of_another_programs_service_and_the_server_out_of_any_unit() {
        let hyprland = below_user_manager(&format!("session.slice/{HYPRLAND}"));
        let gnome = below_user_manager(&format!("app.slice/{GNOME}"));
        let autostart = below_user_manager("app.slice/app-steno\\x2ddesktop@autostart.service");
        let uwsm = below_user_manager(
            "app.slice/app-graphical.slice/app-Hyprland-steno\\x2ddesktop-1a2b3c4d.scope",
        );
        let session = "0::/user.slice/user-1000.slice/session-3.scope\n";
        let servers = || vec![60, 77];
        assert_eq!(plan(&hyprland, servers()), Plan::Leave(HYPRLAND, servers()));
        assert_eq!(plan(&hyprland, vec![]), Plan::Leave(HYPRLAND, vec![]));
        assert_eq!(plan(&gnome, servers()), Plan::MoveServers(servers(), GNOME));
        assert_eq!(
            plan(&autostart, servers()),
            Plan::MoveServers(servers(), "app-steno\\x2ddesktop@autostart.service")
        );
        assert_eq!(
            plan(&uwsm, vec![77]),
            Plan::MoveServers(vec![77], "app-Hyprland-steno\\x2ddesktop-1a2b3c4d.scope")
        );
        assert_eq!(plan(session, servers()), Plan::ServersStay(servers()));
        for stays in [gnome.as_str(), autostart.as_str(), uwsm.as_str(), session] {
            assert_eq!(plan(stays, vec![]), Plan::Stay, "{stays}");
        }
        for stays in [
            "0::/system.slice/display-manager.service\n",
            "0::/\n",
            "0::/user.slice/user-1000.slice/session-3.scope/sub\n",
            "0::/user.slice/user-1000.slice/session-.scope\n",
            "0::/machine.slice/session-3.scope\n",
            "",
        ] {
            assert_eq!(plan(stays, servers()), Plan::Stay, "{stays}");
        }
    }

    /// A mount server already in its scope is not moved again; one whose
    /// cgroup cannot be read is tried.
    #[test]
    fn a_mount_server_in_its_scope_already_is_not_moved_again() {
        let cgroup_of: CgroupOf = |pid| match pid {
            60 => Ok(below_user_manager(&format!(
                "app.slice/{}",
                server_scope_name(60)
            ))),
            61 => Ok(below_user_manager(
                "app.slice/app-steno\\x2ddesktop\\x2dimage-.scope",
            )),
            62 => Ok(below_user_manager(&format!(
                "app.slice/{}/sub",
                server_scope_name(62)
            ))),
            77 => Ok(below_user_manager(&format!("session.slice/{HYPRLAND}"))),
            _ => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        };
        assert_eq!(
            unmoved(vec![60, 61, 62, 77, 78], cgroup_of),
            [61, 62, 77, 78]
        );
    }

    /// One `StartTransientUnit` call.
    type Started = (
        String,
        String,
        Vec<(String, OwnedValue)>,
        Vec<(String, Vec<(String, OwnedValue)>)>,
    );

    const JOB: &str = "/org/freedesktop/systemd1/job/4711";
    const UNIT: &str = "/org/freedesktop/systemd1/unit/wayland_2dwm_40hyprland_2edesktop_2eservice";

    /// What the fake manager was asked.
    #[derive(Default)]
    struct Asked {
        units: Mutex<Vec<String>>,
        started: Mutex<Vec<Started>>,
        cancelled: AtomicUsize,
        stopped: Mutex<Vec<(String, String)>>,
        refuse_app: AtomicBool,
    }

    /// The user manager as far as the move goes: every start queues
    /// [`JOB`], and the compositor's service has a main process.
    struct FakeManager(&'static Asked);
    struct FakeJob(&'static Asked, &'static str);
    struct FakeService(u32);

    #[zbus::interface(name = "org.freedesktop.systemd1.Manager")]
    impl FakeManager {
        fn get_unit(&self, name: String) -> OwnedObjectPath {
            self.0.units.lock().unwrap().push(name);
            OwnedObjectPath::try_from(UNIT).unwrap()
        }

        fn start_transient_unit(
            &self,
            name: String,
            mode: String,
            properties: Vec<(String, OwnedValue)>,
            auxiliary: Vec<(String, Vec<(String, OwnedValue)>)>,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            if self.0.refuse_app.load(Ordering::SeqCst) && name == scope_name(4242) {
                return Err(zbus::fdo::Error::Failed("refused".to_owned()));
            }
            self.0
                .started
                .lock()
                .unwrap()
                .push((name, mode, properties, auxiliary));
            Ok(OwnedObjectPath::try_from(JOB).unwrap())
        }

        fn stop_unit(&self, name: String, mode: String) -> OwnedObjectPath {
            self.0.stopped.lock().unwrap().push((name, mode));
            OwnedObjectPath::try_from("/org/freedesktop/systemd1/job/2").unwrap()
        }
    }

    #[zbus::interface(name = "org.freedesktop.systemd1.Job")]
    impl FakeJob {
        fn cancel(&self) {
            self.0.cancelled.fetch_add(1, Ordering::SeqCst);
        }

        #[zbus(property)]
        fn state(&self) -> String {
            self.1.to_owned()
        }
    }

    #[zbus::interface(name = "org.freedesktop.systemd1.Service")]
    impl FakeService {
        #[zbus(property, name = "MainPID")]
        fn main_pid(&self) -> u32 {
            self.0
        }
    }

    /// The fake on a bus of its own under the manager's name.
    fn fake(
        queued: bool,
        main_pid: Option<u32>,
    ) -> Option<(Daemon, &'static Asked, Connection, Connection)> {
        let daemon = Daemon::start()?;
        let asked: &'static Asked = Box::leak(Box::default());
        let mut manager = daemon
            .builder()
            .name(SYSTEMD)
            .unwrap()
            .serve_at(SYSTEMD_PATH, FakeManager(asked))
            .unwrap();
        if queued {
            manager = manager.serve_at(JOB, FakeJob(asked, "waiting")).unwrap();
        }
        if let Some(pid) = main_pid {
            manager = manager.serve_at(UNIT, FakeService(pid)).unwrap();
        }
        let (manager, app) = (manager.build().unwrap(), daemon.connect());
        Some((daemon, asked, manager, app))
    }

    /// Nothing was called off or stopped.
    fn nothing_undone(asked: &Asked) {
        assert_eq!(asked.cancelled.load(Ordering::SeqCst), 0);
        assert!(asked.stopped.lock().unwrap().is_empty());
    }

    /// `f` on a thread of its own, failing the test after 10 s.
    fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (done, outcome) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(f());
        });
        outcome
            .recv_timeout(Duration::from_secs(10))
            .expect("returned within 10 s")
    }

    /// [`move_out`] for the app 4242 in Hyprland's unit, bounded.
    fn move_app(app: &Connection, servers: &[u32], cgroup_of: CgroupOf) -> Outcome {
        let (app, servers) = (app.clone(), servers.to_vec());
        bounded(move || move_out(&app, HYPRLAND, 4242, &servers, cgroup_of, in_time()))
    }

    /// Every process once the manager moved it.
    const MOVED: CgroupOf = |pid| {
        Ok(below_user_manager(&if pid == 4242 {
            format!("app.slice/app-graphical.slice/{}", scope_name(pid))
        } else {
            format!("app.slice/{}", server_scope_name(pid))
        }))
    };

    /// Every process when the manager never moved it.
    const LEFT_BEHIND: CgroupOf = |_| Ok(below_user_manager(&format!("session.slice/{HYPRLAND}")));

    /// A deadline the test lets pass: the start stays queued.
    fn soon() -> Instant {
        Instant::now() + Duration::from_millis(300)
    }

    /// A deadline a move that succeeds meets on a loaded machine.
    fn in_time() -> Instant {
        Instant::now() + Duration::from_secs(3)
    }

    fn property(properties: &[(String, OwnedValue)], key: &str) -> OwnedValue {
        let (_, value) = properties.iter().find(|(name, _)| name == key).unwrap();
        value.try_clone().unwrap()
    }

    /// The scopes started, each with its `Before=`.
    fn starts(asked: &Asked) -> Vec<(String, Option<Vec<String>>)> {
        let started = asked.started.lock().unwrap();
        started
            .iter()
            .map(|(name, _, properties, _)| {
                let before = properties
                    .iter()
                    .any(|(key, _)| key == "Before")
                    .then(|| Vec::<String>::try_from(property(properties, "Before")).unwrap());
                (name.clone(), before)
            })
            .collect()
    }

    fn servers_before(servers: &[u32], unit: &str) -> Vec<(String, Option<Vec<String>>)> {
        servers
            .iter()
            .map(|&server| (server_scope_name(server), Some(vec![unit.to_owned()])))
            .collect()
    }

    fn app_and_servers(servers: &[u32]) -> Vec<(String, Option<Vec<String>>)> {
        let mut expected = vec![(scope_name(4242), None)];
        expected.extend(servers_before(servers, &scope_name(4242)));
        expected
    }

    /// [`carry_out`] for the app that stays in [`GNOME`]: what it logged.
    fn servers_moved_from_gnome(
        connect: impl FnOnce() -> Result<Connection, MoveError> + Send + 'static,
        cgroup_of: CgroupOf,
    ) -> String {
        let ((), logged) = warnings(|| {
            carry_out(
                Plan::MoveServers(vec![60, 77], GNOME),
                4242,
                connect,
                cgroup_of,
                in_time(),
            )
            .expect("a thread moves the servers")
            .join()
            .unwrap();
        });
        logged
    }

    #[test]
    fn the_app_asks_for_a_scope_of_its_own_in_the_app_slice() {
        let Some((_daemon, asked, _manager, app)) = fake(true, Some(7)) else {
            return;
        };

        let moved = move_app(&app, &[], MOVED).unwrap();
        assert_eq!(moved.as_deref(), Some("app-steno\\x2ddesktop-4242.scope"));
        assert_eq!(*asked.units.lock().unwrap(), [HYPRLAND]);
        let started = asked.started.lock().unwrap();
        let [(name, mode, properties, auxiliary)] = started.as_slice() else {
            panic!("one start: {}", started.len());
        };
        assert_eq!(
            (name.as_str(), mode.as_str()),
            ("app-steno\\x2ddesktop-4242.scope", "fail")
        );
        assert_eq!(auxiliary.len(), 0);
        assert_eq!(
            Vec::<u32>::try_from(property(properties, "PIDs")).unwrap(),
            [4242]
        );
        assert_eq!(
            String::try_from(property(properties, "Slice")).unwrap(),
            "app-graphical.slice"
        );
        assert_eq!(
            u64::try_from(property(properties, "TimeoutStopUSec")).unwrap(),
            20_000_000
        );
        for key in ["PartOf", "After"] {
            assert_eq!(
                Vec::<String>::try_from(property(properties, key)).unwrap(),
                ["graphical-session.target"],
                "{key}"
            );
        }
        assert_eq!(
            String::try_from(property(properties, "CollectMode")).unwrap(),
            "inactive-or-failed"
        );
        assert_eq!(properties.len(), 7);
        nothing_undone(asked);
    }

    #[test]
    fn the_mount_server_gets_a_scope_of_its_own_that_no_session_end_stops() {
        let Some((_daemon, asked, _manager, app)) = fake(true, Some(7)) else {
            return;
        };

        let moved = move_app(&app, &[60, 77], MOVED).unwrap();
        assert_eq!(moved, Some(scope_name(4242)));
        assert_eq!(starts(asked), app_and_servers(&[60, 77]));
        let started = asked.started.lock().unwrap();
        let [_, _, (name, mode, properties, auxiliary)] = started.as_slice() else {
            panic!("three starts: {}", started.len());
        };
        assert_eq!(
            (name.as_str(), mode.as_str()),
            ("app-steno\\x2ddesktop\\x2dimage-77.scope", "fail")
        );
        assert_eq!(auxiliary.len(), 0);
        assert_eq!(
            Vec::<u32>::try_from(property(properties, "PIDs")).unwrap(),
            [77]
        );
        assert_eq!(
            String::try_from(property(properties, "Slice")).unwrap(),
            "app.slice"
        );
        assert_eq!(
            Vec::<String>::try_from(property(properties, "Before")).unwrap(),
            [scope_name(4242)]
        );
        assert_eq!(
            String::try_from(property(properties, "CollectMode")).unwrap(),
            "inactive-or-failed"
        );
        assert_eq!(properties.len(), 5);
        nothing_undone(asked);
    }

    #[test]
    fn the_services_main_process_stays() {
        let Some((_daemon, asked, _manager, app)) = fake(true, Some(4242)) else {
            return;
        };
        assert!(matches!(move_app(&app, &[], MOVED), Ok(None)));
        assert!(asked.started.lock().unwrap().is_empty());
    }

    /// An app that stays still moves every mount server, each ordered
    /// before the unit it stays in.
    #[test]
    fn the_mount_servers_leave_whenever_the_app_stays() {
        let image = || servers_before(&[60, 77], HYPRLAND);

        let Some((_daemon, asked, _manager, app)) = fake(true, Some(4242)) else {
            return;
        };
        assert!(matches!(move_app(&app, &[60, 77], MOVED), Ok(None)));
        assert_eq!(starts(asked), image());

        let Some((_daemon, asked, _manager, app)) = fake(true, None) else {
            return;
        };
        let error = move_app(&app, &[60, 77], MOVED).unwrap_err();
        assert!(matches!(error, MoveError::Bus(_)), "{error}");
        assert_eq!(starts(asked), image());

        let Some((_daemon, asked, _manager, app)) = fake(true, Some(7)) else {
            return;
        };
        asked.refuse_app.store(true, Ordering::SeqCst);
        let error = move_app(&app, &[60, 77], MOVED).unwrap_err();
        assert!(matches!(error, MoveError::Bus(_)), "{error}");
        assert_eq!(starts(asked), image());
        nothing_undone(asked);
    }

    /// A mount server's move counts once its cgroup names its scope;
    /// anything else is an error.
    #[test]
    fn the_mount_servers_move_is_checked() {
        let unit = "app-steno\\x2ddesktop@autostart.service";
        let image = |app: &Connection, cgroup_of: CgroupOf, deadline: Instant| {
            let app = app.clone();
            bounded(move || move_server(&app, 77, unit, cgroup_of, deadline))
        };

        let Some((_daemon, asked, _manager, app)) = fake(true, Some(7)) else {
            return;
        };
        image(&app, MOVED, in_time()).unwrap();
        assert_eq!(starts(asked), servers_before(&[77], unit));
        assert!(asked.units.lock().unwrap().is_empty());
        let error = image(&app, LEFT_BEHIND, soon()).unwrap_err();
        assert!(matches!(error, MoveError::StillQueued), "{error}");
        nothing_undone(asked);

        let Some((_daemon, _asked, _manager, app)) = fake(false, Some(7)) else {
            return;
        };
        let error = image(&app, LEFT_BEHIND, in_time()).unwrap_err();
        assert!(matches!(error, MoveError::NotMoved), "{error}");
        image(&app, MOVED, in_time()).unwrap();

        let Some(daemon) = Daemon::start() else {
            return;
        };
        let error = image(&daemon.connect(), MOVED, in_time()).unwrap_err();
        assert!(matches!(error, MoveError::Bus(_)), "{error}");
    }

    /// The servers named by the warn-level "stays" lines, in order.
    fn stayed(logged: &str) -> Vec<&str> {
        logged
            .lines()
            .filter(|line| line.contains("mount server stays"))
            .map(|line| {
                assert!(line.contains("WARN"), "{line}");
                line.split_once(" server=")
                    .and_then(|(_, rest)| rest.split(' ').next())
                    .unwrap_or("")
            })
            .collect()
    }

    /// Each plan's arm moves the mount servers it names, logging nothing.
    #[test]
    fn each_plan_moves_the_mount_servers_it_names() {
        let Some((_daemon, asked, _manager, app)) = fake(true, Some(7)) else {
            return;
        };
        let connection = app.clone();
        let (moved, logged) = warnings(|| {
            carry_out(
                Plan::Leave(HYPRLAND, vec![60, 77]),
                4242,
                move || Ok(connection),
                MOVED,
                in_time(),
            )
        });
        assert!(moved.is_none());
        assert_eq!(starts(asked), app_and_servers(&[60, 77]));
        assert_eq!(logged, "");

        let Some((_daemon, asked, _manager, app)) = fake(true, Some(7)) else {
            return;
        };
        let logged = servers_moved_from_gnome(move || Ok(app), MOVED);
        assert_eq!(starts(asked), servers_before(&[60, 77], GNOME));
        assert_eq!(logged, "");
        nothing_undone(asked);

        let ((), logged) = warnings(|| {
            let none = carry_out(
                Plan::Stay,
                4242,
                || Err(MoveError::NotMoved),
                MOVED,
                in_time(),
            );
            assert!(none.is_none());
        });
        assert_eq!(logged, "");
    }

    /// Every mount server that stays where it runs is logged at warn.
    #[test]
    fn every_mount_server_that_stays_is_logged() {
        let no_bus = || Err(MoveError::NoRuntimeDir);

        let (_, logged) = warnings(|| {
            carry_out(
                Plan::Leave(HYPRLAND, vec![60, 77]),
                4242,
                no_bus,
                MOVED,
                in_time(),
            )
        });
        assert_eq!(stayed(&logged), ["60", "77"], "{logged}");
        assert!(
            logged
                .lines()
                .any(|line| line.contains("WARN") && line.contains("uwsm-app")),
            "{logged}"
        );

        let logged = servers_moved_from_gnome(no_bus, MOVED);
        assert_eq!(stayed(&logged), ["60", "77"], "{logged}");

        let Some((_daemon, _asked, _manager, app)) = fake(false, Some(7)) else {
            return;
        };
        let logged = servers_moved_from_gnome(move || Ok(app), LEFT_BEHIND);
        assert_eq!(stayed(&logged), ["60", "77"], "{logged}");
        assert!(logged.contains("without the move"), "{logged}");

        let (none, logged) = warnings(|| {
            carry_out(
                Plan::ServersStay(vec![60, 77]),
                4242,
                no_bus,
                MOVED,
                in_time(),
            )
        });
        assert!(none.is_none());
        assert_eq!(stayed(&logged), ["60", "77"], "{logged}");
        assert!(logged.contains("session scope"), "{logged}");
    }

    /// An app that stays waits for each mount server's move until the
    /// launch's deadline.
    #[test]
    fn a_mount_server_seen_in_its_scope_on_a_later_read_is_moved() {
        static READS: AtomicUsize = AtomicUsize::new(0);
        let Some((_daemon, asked, _manager, app)) = fake(true, Some(7)) else {
            return;
        };
        let after_a_read: CgroupOf = |pid| {
            if READS.fetch_add(1, Ordering::SeqCst) == 0 {
                LEFT_BEHIND(pid)
            } else {
                MOVED(pid)
            }
        };

        let logged = servers_moved_from_gnome(move || Ok(app), after_a_read);
        assert_eq!(logged, "");
        assert_eq!(starts(asked), servers_before(&[60, 77], GNOME));
        assert_eq!(READS.load(Ordering::SeqCst), 3);
        nothing_undone(asked);
    }

    /// Set in the copy of this test binary whose stderr a test reads.
    const STDERR_CHILD: &str = "STENO_TEST_OWN_SCOPE_STDERR_CHILD";

    /// What the test `name` wrote to stderr, run in a copy of this binary;
    /// `None` without `dbus-daemon`.
    fn stderr_of(name: &str) -> Option<String> {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env(STDERR_CHILD, "1")
            .stdout(std::process::Stdio::null())
            .output()
            .unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{}: {stderr}", output.status);
        (!stderr.contains("SKIPPED: dbus-daemon")).then_some(stderr)
    }

    /// The launch moves only the mount servers not in their scope yet, and
    /// each move is written to stderr.
    #[test]
    fn the_launch_moves_the_mount_servers_found_and_writes_each_move_to_stderr() {
        static READS: AtomicUsize = AtomicUsize::new(0);
        if std::env::var_os(STDERR_CHILD).is_none() {
            let Some(stderr) = stderr_of(
                "own_scope::tests::the_launch_moves_the_mount_servers_found_and_writes_each_move_to_stderr",
            ) else {
                return;
            };
            let moves: Vec<&str> = stderr
                .lines()
                .filter(|line| line.contains("scope of its own"))
                .collect();
            assert_eq!(
                moves,
                [
                    "[steno-desktop] moved the AppImage's mount server 77 into a scope of its own, app-steno\\x2ddesktop\\x2dimage-77.scope, which the session's end does not stop",
                    "[steno-desktop] started inside wayland-wm@hyprland.desktop.service and moved into a scope of its own, app-steno\\x2ddesktop-4242.scope, where a save at the session's end has 20 s",
                ],
                "{stderr}"
            );
            return;
        }

        let Some((_daemon, asked, _manager, app)) = fake(true, Some(7)) else {
            return;
        };
        let cgroup_of: CgroupOf = |pid| match pid {
            4242 => Ok(below_user_manager(&format!("app.slice/{GNOME}"))),
            77 if READS.fetch_add(1, Ordering::SeqCst) == 0 => LEFT_BEHIND(pid),
            _ => MOVED(pid),
        };
        let leaving = app.clone();
        launch(4242, || vec![60, 77], move || Ok(app), cgroup_of, in_time())
            .expect("a thread moves the servers")
            .join()
            .unwrap();
        assert_eq!(starts(asked), servers_before(&[77], GNOME));

        let none = carry_out(
            Plan::Leave(HYPRLAND, vec![]),
            4242,
            move || Ok(leaving),
            MOVED,
            in_time(),
        );
        assert!(none.is_none());
        let mut expected = servers_before(&[77], GNOME);
        expected.push((scope_name(4242), None));
        assert_eq!(starts(asked), expected);
        nothing_undone(asked);
    }

    #[test]
    fn a_service_whose_main_process_is_unknown_starts_nothing() {
        let Some((_daemon, asked, _manager, app)) = fake(true, None) else {
            return;
        };

        let error = move_app(&app, &[], MOVED).unwrap_err();
        assert!(matches!(error, MoveError::Bus(_)), "{error}");
        assert!(asked.started.lock().unwrap().is_empty());
    }

    #[test]
    fn an_app_seen_in_its_scope_after_a_few_reads_is_moved() {
        static READS: AtomicUsize = AtomicUsize::new(0);
        let Some((_daemon, asked, _manager, app)) = fake(true, Some(7)) else {
            return;
        };
        let after_three_reads: CgroupOf = |pid| {
            if READS.fetch_add(1, Ordering::SeqCst) < 3 {
                LEFT_BEHIND(pid)
            } else {
                MOVED(pid)
            }
        };

        let moved = move_app(&app, &[], after_three_reads).unwrap();
        assert_eq!(moved, Some(scope_name(4242)));
        assert_eq!(READS.load(Ordering::SeqCst), 4);
        nothing_undone(asked);
    }

    /// A start queued at login stays queued past the deadline.
    #[test]
    fn a_start_still_queued_at_the_deadline_stays_queued() {
        let Some((_daemon, asked, _manager, app)) = fake(true, Some(7)) else {
            return;
        };

        let started = Instant::now();
        let error =
            bounded(move || move_out(&app, HYPRLAND, 4242, &[], LEFT_BEHIND, soon())).unwrap_err();
        assert!(matches!(error, MoveError::StillQueued), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
        nothing_undone(asked);
    }

    #[test]
    fn a_job_that_ended_without_the_app_stops_nothing() {
        static JOINED: AtomicBool = AtomicBool::new(false);
        let Some((_daemon, asked, _manager, app)) = fake(false, Some(7)) else {
            return;
        };

        let error = move_app(&app, &[], LEFT_BEHIND).unwrap_err();
        assert!(matches!(error, MoveError::NotMoved), "{error}");

        // The job moved the app just before it ended.
        let joins_as_the_job_ends: CgroupOf = |pid| {
            if JOINED.swap(true, Ordering::SeqCst) {
                MOVED(pid)
            } else {
                LEFT_BEHIND(pid)
            }
        };
        let moved = move_app(&app, &[], joins_as_the_job_ends).unwrap();
        assert_eq!(moved, Some(scope_name(4242)));
        nothing_undone(asked);
    }

    /// The bus answers for a manager no one runs.
    #[test]
    fn a_bus_without_a_user_manager_is_an_error() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let error = move_app(&daemon.connect(), &[], MOVED).unwrap_err();
        assert!(
            matches!(&error, MoveError::Bus(zbus::Error::MethodError(name, ..)) if name.starts_with("org.freedesktop.DBus.Error.")),
            "{error}"
        );
    }

    /// A runtime directory of its own, removed with the value.
    struct RuntimeDir(std::path::PathBuf);

    impl RuntimeDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("steno-own-scope-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for RuntimeDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The user bus is the socket in the runtime directory (absolute).
    #[test]
    fn the_user_bus_is_the_runtime_directorys_socket() {
        let runtime = RuntimeDir::new("bus");
        let Some(_daemon) =
            Daemon::start_at(&format!("unix:path={}", runtime.0.join("bus").display()))
        else {
            return;
        };
        let connection = bus_in(Some(&runtime.0)).unwrap();
        assert!(connection.unique_name().is_some());

        assert!(matches!(bus_in(None), Err(MoveError::NoRuntimeDir)));
        assert!(matches!(
            bus_in(Some(Path::new("relative/run"))),
            Err(MoveError::NoRuntimeDir)
        ));
        let empty = RuntimeDir::new("empty");
        assert!(matches!(bus_in(Some(&empty.0)), Err(MoveError::NoBus(_))));
    }

    /// A manager that never answers ends the request within the timeout.
    #[test]
    fn a_silent_user_manager_ends_the_request_in_time() {
        let runtime = RuntimeDir::new("silent");
        let Some(daemon) =
            Daemon::start_at(&format!("unix:path={}", runtime.0.join("bus").display()))
        else {
            return;
        };
        let _silent = daemon.builder().name(SYSTEMD).unwrap().build().unwrap();
        let app = bus_in(Some(&runtime.0)).unwrap();

        let started = Instant::now();
        let error = move_app(&app, &[], MOVED).unwrap_err();
        assert!(matches!(error, MoveError::Bus(_)), "{error}");
        assert!(started.elapsed() < CALL_TIMEOUT + Duration::from_secs(2));
    }

    /// The launch waits a bounded time and goes on while the request runs.
    #[test]
    fn the_launch_goes_on_while_the_move_waits() {
        let moved = move_within(
            Duration::from_secs(5),
            || Ok(Some(scope_name(4242))),
            |_| panic!("handed over in time"),
        );
        assert_eq!(moved.unwrap(), Some(scope_name(4242)));

        let started = Instant::now();
        let waiting = bounded(|| {
            move_within(
                Duration::from_millis(100),
                || {
                    loop {
                        std::thread::park();
                    }
                },
                |_| {},
            )
        });
        assert!(matches!(waiting, Err(MoveError::Waiting)), "{waiting:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// A late outcome reaches the late report once; a timely one never does.
    #[test]
    fn a_late_outcome_is_reported_once() {
        let (reported, late) = mpsc::channel();
        let early = reported.clone();
        let waiting = move_within(
            Duration::from_millis(50),
            || {
                std::thread::sleep(Duration::from_millis(300));
                Ok(Some(scope_name(4242)))
            },
            move |outcome| reported.send(outcome).unwrap(),
        );
        assert!(matches!(waiting, Err(MoveError::Waiting)), "{waiting:?}");
        let outcome = late.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(outcome.unwrap(), Some(scope_name(4242)));

        let moved = move_within(
            Duration::from_secs(5),
            || Ok(None),
            move |outcome| early.send(outcome).unwrap(),
        );
        assert!(matches!(moved, Ok(None)));
        // Both senders are gone once the threads end: nothing more came.
        assert!(matches!(
            late.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }
}
