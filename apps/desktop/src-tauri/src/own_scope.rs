//! Linux only: the app in a systemd scope of its own when it starts inside
//! another program's service (X1 of `.plans/2026-10-07-stable-promotion.md`).
//!
//! A program that starts Steno without a unit of its own leaves it in its
//! own cgroup. On Hyprland under uwsm, a key binding's `exec` without
//! `uwsm-app` runs Steno inside the compositor's
//! `wayland-wm@hyprland.desktop.service`: stopping that unit sends SIGTERM
//! to Steno together with the compositor and, once the compositor has
//! exited, a second SIGTERM to every process left in the unit, which ends
//! Steno unsaved (`forced_exit`); `TimeoutStopSec=10` would cut off a
//! longer save too. So at launch, before the first window starts the web
//! view's processes, the app reads `/proc/self/cgroup`. When it names a
//! service of the systemd user manager that is not Steno's own (its name
//! does not say `steno`) and whose main process is another, the app asks
//! the user manager for a transient scope that holds only itself,
//! `app-steno\x2ddesktop-<pid>.scope` in `app-graphical.slice`, with
//! `TimeoutStopSec=20s` (as `stop_timeout` gives the autostart unit and
//! GNOME's scope), `PartOf=` and `After=graphical-session.target`. The
//! session's end then stops the scope before the compositor, with the
//! display still up, and waits 20 s for the save; the web view's processes
//! and the speech sidecar start in the scope later.
//!
//! Unlike the speech sidecar's move (`crates/steno-speech/src/sidecar/scope.rs`),
//! a start that is still queued is never called off: at login (`exec-once`)
//! it waits behind `graphical-session.target`, and the launch stops waiting
//! after [`MOVE_TIMEOUT`] while the request's thread goes on, up to
//! [`QUEUE_LIMIT`], and logs the outcome itself. A session that ends before
//! the target is reached cancels the start and leaves Steno in the
//! compositor's unit. Nothing here stops a scope.
//!
//! Started from an `AppImage`, the app reads its files from the image's
//! mount, which the `AppImage` runtime's own process, the mount server,
//! serves (`appimage`). It runs in the unit the app was started in, or in
//! the launcher's when the launcher moves only the app into a scope after
//! the spawn (GNOME): the stop of that unit (the compositor's, a `uwsm-app`
//! scope, GNOME's scope, the autostart unit, a user's own service) would
//! end it while the app saves, and the app with it. Wherever the app runs
//! in the user manager, the mount server gets a scope of its own,
//! `app-steno\x2ddesktop\x2dimage-<pid>.scope` in `app.slice`, which no
//! session's end stops; it ends by itself when the app exits, and an exit
//! of the user manager stops it after the app's unit. Its move is checked
//! as the app's is, and a failure is logged. In a login's
//! `session-<n>.scope` (Hyprland without uwsm), outside the user manager,
//! the mount server stays beside the app, with a warning: that scope's
//! stop ends both.
//!
//! A scope (including another program's, which the app stays in), Steno's
//! own service, and an app outside the user manager (a system service, an
//! ssh login, a container, no systemd) stay where they are, silently. A
//! user's own service whose shell does not `exec` Steno counts as another
//! program's; the move does no harm there. The move is written to stderr;
//! an app that does not move logs a warning with the unit it runs in
//! afterwards, since the save at the session's end may be cut off there,
//! and a move still waiting when the launch goes on is logged when it
//! ends. The request goes over the user bus's socket in
//! `$XDG_RUNTIME_DIR` and carries the pids, the scopes' names and their
//! fixed settings, nothing else.
//!
//! - [`leave_foreign_service`]: the one entry point, called first in
//!   `setup`.
//! - [`plan`]: what moves, from the cgroup and the mount server.
//! - [`move_within`]: the request on a thread of its own, waited for at
//!   most [`MOVE_TIMEOUT`].
//! - [`user_bus`]: the connection to the user bus's socket.
//! - [`move_out`]: the main-process check, the scopes' starts and the wait.
//! - [`move_image`]: the mount server's scope, started and checked.
//! - [`foreign_service`]: the unit to leave, from the cgroup.
//!
//! Swift: none; the Mac app is never in another program's unit.

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
/// twice uwsm's `TimeoutStartSec=30` for the compositor's unit, which
/// bounds how long `graphical-session.target` can take to be reached.
const QUEUE_LIMIT: Duration = Duration::from_secs(60);

/// How long each call to the user manager may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// How often the app's cgroup and the manager's job are looked at.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The scope's stop timeout: the drop-ins' 20 s (`stop_timeout`), at least
/// half again the save and the process's end.
const STOP_TIMEOUT: Duration = Duration::from_secs(20);

// systemd's manager: its bus name, object path and interfaces.
const SYSTEMD: &str = "org.freedesktop.systemd1";
const SYSTEMD_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MANAGER: &str = "org.freedesktop.systemd1.Manager";
const SYSTEMD_JOB: &str = "org.freedesktop.systemd1.Job";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// Why the app is not in a scope of its own.
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
    #[error("the user manager had not started the scope after {}s; its start stays queued", QUEUE_LIMIT.as_secs())]
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

/// The unit a cgroup file puts its process in: the path's last part.
fn innermost(cgroup: &str) -> Option<&str> {
    unified(cgroup)?.rsplit('/').next()
}

/// The service of another program the app runs in, from its
/// `/proc/self/cgroup`: a service of a systemd user manager (a path through
/// `user@<uid>.service`) whose name does not say `steno`. `None` for a scope,
/// Steno's own service (the autostart unit, `uwsm-app -t service`'s, a user's
/// `steno.service`), a sub-cgroup inside a unit, a path outside a user
/// manager, and cgroup v1, which has no `0::` line.
fn foreign_service(cgroup: &str) -> Option<&str> {
    let unit = user_unit(cgroup, ".service")?;
    (!unit.to_ascii_lowercase().contains("steno")).then_some(unit)
}

/// The unit of a systemd user manager (a path through `user@<uid>.service`)
/// that a cgroup file puts its process in, when its name ends in `suffix`
/// and is a unit's: not a sub-cgroup, not an escaped (`_`) one.
fn user_unit<'a>(cgroup: &'a str, suffix: &str) -> Option<&'a str> {
    let mut parts = unified(cgroup)?.rsplit('/');
    let unit = parts.next()?;
    let in_user_manager = parts.any(|part| part.starts_with("user@") && part.ends_with(".service"));
    let named = unit
        .strip_suffix(suffix)
        .is_some_and(|stem| !stem.is_empty() && !stem.starts_with('_'));
    (in_user_manager && named).then_some(unit)
}

/// The app's own unit in the user manager, a scope or a service: what the
/// mount server's scope is ordered against when the app stays.
fn own_unit(cgroup: &str) -> Option<&str> {
    user_unit(cgroup, ".scope").or_else(|| user_unit(cgroup, ".service"))
}

/// Whether a cgroup file puts its process in a login's
/// `session-<n>.scope`, outside the user manager.
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
    /// The app leaves another program's service, and the mount server, when
    /// there is one, leaves with it.
    Leave(&'a str),
    /// The app stays in its own unit, and the mount server (the pid) leaves
    /// whatever unit it runs in, ordered before the app's.
    MoveServer(u32, &'a str),
    /// The app runs in a login's session scope, outside the user manager,
    /// and the mount server (the pid) stays beside it.
    ServerStays(u32),
    /// Nothing moves.
    Stay,
}

/// What the launch moves, from the app's cgroup file and the `AppImage`'s
/// mount server, when the app runs from one.
fn plan(cgroup: &str, server: Option<u32>) -> Plan<'_> {
    if let Some(unit) = foreign_service(cgroup) {
        return Plan::Leave(unit);
    }
    match (server, own_unit(cgroup)) {
        (Some(server), Some(unit)) => Plan::MoveServer(server, unit),
        (Some(server), None) if in_login_session(cgroup) => Plan::ServerStays(server),
        _ => Plan::Stay,
    }
}

/// The app's scope, named as the XDG convention for applications' units has
/// it (`app-<id>-<random>.scope`), the pid as the random part.
fn scope_name(pid: u32) -> String {
    format!("app-steno\\x2ddesktop-{pid}.scope")
}

/// The scope of the mount server `server`, named as [`scope_name`] names
/// the app's.
fn image_scope_name(server: u32) -> String {
    format!("app-steno\\x2ddesktop\\x2dimage-{server}.scope")
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
        // A scope whose app was killed ends failed; it is collected then,
        // not kept for `systemctl --user --failed`.
        ("CollectMode", Value::from("inactive-or-failed")),
    ]
}

/// What the mount server's scope is started with: outside the graphical
/// slices, stopped after the app's unit `app`.
fn image_properties(server: u32, app: &str) -> Vec<(&'static str, Value<'static>)> {
    vec![
        ("Description", Value::from("Steno's AppImage mount")),
        ("PIDs", Value::from(vec![server])),
        ("Slice", Value::from("app.slice")),
        ("Before", Value::from(vec![app.to_owned()])),
        ("CollectMode", Value::from("inactive-or-failed")),
    ]
}

/// Calls `method` of `interface` at `path` of the user manager, by its bus name.
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

/// The main process of the service `unit`, from the user manager.
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

/// Starts the transient scope `name` with `properties`; the job it queued.
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
/// the app's unit `app`, once `cgroup_of` names that scope for it, the
/// job has ended, or `deadline` passes.
fn move_image(
    connection: &Connection,
    server: u32,
    app: &str,
    cgroup_of: impl Fn(u32) -> std::io::Result<String>,
    deadline: Instant,
) -> Result<(), MoveError> {
    let scope = image_scope_name(server);
    let job = start_scope(connection, &scope, image_properties(server, app))?;
    joined(
        connection,
        &job,
        || in_unit(&cgroup_of, server, &scope),
        deadline,
    )
}

/// [`move_image`], a failure logged.
fn take_image(
    connection: &Connection,
    server: u32,
    app: &str,
    cgroup_of: impl Fn(u32) -> std::io::Result<String>,
    deadline: Instant,
) {
    if let Err(error) = move_image(connection, server, app, cgroup_of, deadline) {
        image_stays(server, &error);
    }
}

/// Logs why the mount server `server` stays where it runs.
fn image_stays(server: u32, error: &dyn std::fmt::Display) {
    tracing::warn!(
        server,
        %error,
        "the AppImage's mount server stays where it runs, and the stop of that unit at the session's end may end it while Steno saves a recording"
    );
}

/// Whether the manager's `job` has ended: the manager no longer knows it.
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

/// Whether `cgroup_of` puts the process `pid` in the unit `name`.
fn in_unit(cgroup_of: impl Fn(u32) -> std::io::Result<String>, pid: u32, name: &str) -> bool {
    cgroup_of(pid).is_ok_and(|cgroup| innermost(&cgroup) == Some(name))
}

/// Waits for the start `job` to move a process: `Ok` once `moved` says so,
/// `NotMoved` once the job has ended without it (`moved` read once more
/// then), and `StillQueued` when `deadline` passes first. Nothing is called
/// off.
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

/// Moves the app `pid`, which runs in another program's service `unit`, into a
/// scope of its own over `connection`, and the mount server `server` into one
/// beside it; the app's scope once `cgroup_of` names it, or `None` when the
/// app is `unit`'s main process and stays.
///
/// The app's scope is started first, so the mount server's can be ordered
/// before it; when the app stays (its service's main process, or a failed
/// call), the mount server's scope is ordered before `unit`. The mount
/// server's move is checked before the app's.
///
/// `StartTransientUnit` answers once the manager queued the job, and the job
/// moves the app later: at once, or at login once `graphical-session.target`
/// is reached. The app's cgroup and the job are looked at until the cgroup
/// names the scope, the job has ended, or `deadline` passes; a job still
/// queued then stays queued and may move the app later. Nothing is called
/// off or stopped: a job that ended without the app (its start failed, or
/// the session's end cancelled it) leaves no scope that holds it.
fn move_out(
    connection: &Connection,
    unit: &str,
    pid: u32,
    server: Option<u32>,
    cgroup_of: impl Fn(u32) -> std::io::Result<String>,
    deadline: Instant,
) -> Outcome {
    let name = scope_name(pid);
    let started = match main_pid(connection, unit) {
        Ok(main) if main == pid => Ok(None),
        Ok(_) => start_scope(connection, &name, properties(pid)).map(Some),
        Err(error) => Err(error),
    };
    if let Some(server) = server {
        let before = if matches!(started, Ok(Some(_))) {
            name.as_str()
        } else {
            unit
        };
        take_image(connection, server, before, &cgroup_of, deadline);
    }
    let Some(job) = started? else {
        return Ok(None);
    };
    joined(
        connection,
        &job,
        || in_unit(&cgroup_of, pid, &name),
        deadline,
    )?;
    Ok(Some(name))
}

/// The cgroup file of the process `pid`.
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

/// A connection to the bus socket `bus` in the runtime directory `runtime`,
/// which has to be an absolute path, each call answered within
/// [`CALL_TIMEOUT`] or failed.
fn bus_in(runtime: Option<&Path>) -> Result<Connection, MoveError> {
    let runtime = runtime
        .filter(|dir| dir.is_absolute())
        .ok_or(MoveError::NoRuntimeDir)?;
    let stream = UnixStream::connect(runtime.join("bus")).map_err(MoveError::NoBus)?;
    Ok(Builder::async_io_unix_stream(stream)
        .method_timeout(CALL_TIMEOUT)
        .build()?)
}

/// Runs `request` on a thread of its own and waits at most `wait` for its
/// outcome; `Waiting` after that. The thread then goes on and hands its
/// outcome to `late` itself: the outcome is handed over only to a receiver
/// still waiting, so it reaches exactly one of the two.
fn move_within(
    wait: Duration,
    request: impl FnOnce() -> Outcome + Send + 'static,
    late: impl FnOnce(Outcome) + Send + 'static,
) -> Outcome {
    let (done, outcome) = mpsc::sync_channel(0);
    let spawned = std::thread::Builder::new()
        .name("steno-own-scope".to_owned())
        .spawn(move || {
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

/// Logs `outcome` for the app that started inside `unit`; `late` when the
/// launch had stopped waiting for it.
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

/// Moves the app into a scope of its own when it runs in another program's
/// service, waiting at most [`MOVE_TIMEOUT`], and the `AppImage`'s mount
/// server out of the unit it runs in (the module docs). Called at launch,
/// before the first window. Never fails: the app records where it is
/// otherwise.
pub fn leave_foreign_service() {
    let pid = std::process::id();
    let Ok(cgroup) = cgroup_of(pid) else {
        return;
    };
    let server = crate::appimage::mount_server(pid);
    let deadline = Instant::now() + QUEUE_LIMIT;
    match plan(&cgroup, server) {
        Plan::Leave(unit) => {
            let (request, late) = (unit.to_owned(), unit.to_owned());
            let outcome = move_within(
                MOVE_TIMEOUT,
                move || {
                    let connection = user_bus().inspect_err(|error| {
                        if let Some(server) = server {
                            image_stays(server, error);
                        }
                    })?;
                    move_out(&connection, &request, pid, server, cgroup_of, deadline)
                },
                move |outcome| report(&late, &outcome, true),
            );
            report(unit, &outcome, false);
        }
        Plan::MoveServer(server, unit) => {
            // On a thread nothing waits for: the app stays either way.
            let app = unit.to_owned();
            let spawned = std::thread::Builder::new()
                .name("steno-image-scope".to_owned())
                .spawn(move || match user_bus() {
                    Ok(connection) => take_image(&connection, server, &app, cgroup_of, deadline),
                    Err(error) => image_stays(server, &error),
                });
            if let Err(error) = spawned {
                image_stays(server, &error);
            }
        }
        Plan::ServerStays(server) => image_stays(
            server,
            &"Steno runs in a login's session scope, outside the user manager",
        ),
        Plan::Stay => {}
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use crate::session_end::tests::Daemon;

    const USER: &str = "0::/user.slice/user-1000.slice/user@1000.service";
    const HYPRLAND: &str = "wayland-wm@hyprland.desktop.service";

    /// A cgroup file below the user manager.
    fn below_user_manager(rest: &str) -> String {
        format!("{USER}/{rest}\n")
    }

    #[test]
    fn only_another_programs_service_in_the_user_manager_is_left() {
        assert_eq!(
            foreign_service(&below_user_manager(&format!("session.slice/{HYPRLAND}"))),
            Some(HYPRLAND)
        );
        // A launcher's service in the app slice; a hybrid system's v1 lines
        // are skipped for the unified one.
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
            // Steno's own services: the autostart unit, `uwsm-app -t service`'s,
            // the NixOS module's.
            below_user_manager("app.slice/app-steno\\x2ddesktop@autostart.service"),
            below_user_manager(
                "app.slice/app-graphical.slice/app-Hyprland-steno\\x2ddesktop@1a2b3c4d.service",
            ),
            below_user_manager("app.slice/Steno.service"),
            // A sub-cgroup inside a unit, an escaped name, no name, the
            // manager itself and its init scope.
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
            // A container's system service below another service, and a
            // path through a `user@` part that is no user manager.
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
            image_scope_name(77),
            "app-steno\\x2ddesktop\\x2dimage-77.scope"
        );
        assert_eq!(
            innermost(&below_user_manager("app.slice/x.scope")),
            Some("x.scope")
        );
        let save = steno_services::app::SHUTDOWN_PATIENCE + crate::EXIT_GRACE;
        assert!(STOP_TIMEOUT >= save * 3 / 2);
        assert!(MOVE_TIMEOUT <= Duration::from_secs(3));
        // Past uwsm's `TimeoutStartSec=30`, which bounds the wait at login.
        assert!(QUEUE_LIMIT > Duration::from_secs(30));
    }

    /// The launch's choice, with GNOME's scope (where the launcher left the
    /// mount server behind) and the autostart unit among the inputs.
    #[test]
    fn the_plan_moves_the_app_out_of_another_programs_service_and_the_server_out_of_any_unit() {
        let hyprland = below_user_manager(&format!("session.slice/{HYPRLAND}"));
        let gnome = below_user_manager("app.slice/app-gnome-steno\\x2ddesktop-4242.scope");
        let autostart = below_user_manager("app.slice/app-steno\\x2ddesktop@autostart.service");
        let uwsm = below_user_manager(
            "app.slice/app-graphical.slice/app-Hyprland-steno\\x2ddesktop-1a2b3c4d.scope",
        );
        let session = "0::/user.slice/user-1000.slice/session-3.scope\n";
        assert_eq!(plan(&hyprland, Some(77)), Plan::Leave(HYPRLAND));
        assert_eq!(plan(&hyprland, None), Plan::Leave(HYPRLAND));
        assert_eq!(
            plan(&gnome, Some(77)),
            Plan::MoveServer(77, "app-gnome-steno\\x2ddesktop-4242.scope")
        );
        assert_eq!(
            plan(&autostart, Some(77)),
            Plan::MoveServer(77, "app-steno\\x2ddesktop@autostart.service")
        );
        assert_eq!(
            plan(&uwsm, Some(77)),
            Plan::MoveServer(77, "app-Hyprland-steno\\x2ddesktop-1a2b3c4d.scope")
        );
        assert_eq!(plan(session, Some(77)), Plan::ServerStays(77));
        for stays in [gnome.as_str(), autostart.as_str(), uwsm.as_str(), session] {
            assert_eq!(plan(stays, None), Plan::Stay, "{stays}");
        }
        // A system service, a container's root, a sub-cgroup, no cgroup v2.
        for stays in [
            "0::/system.slice/display-manager.service\n",
            "0::/\n",
            "0::/user.slice/user-1000.slice/session-3.scope/sub\n",
            "0::/user.slice/user-1000.slice/session-.scope\n",
            "",
        ] {
            assert_eq!(plan(stays, Some(77)), Plan::Stay, "{stays}");
        }
    }

    /// One `StartTransientUnit` call: the name, the mode, the properties
    /// and the auxiliary units.
    type Started = (
        String,
        String,
        Vec<(String, OwnedValue)>,
        Vec<(String, Vec<(String, OwnedValue)>)>,
    );

    const JOB: &str = "/org/freedesktop/systemd1/job/4711";
    const UNIT: &str = "/org/freedesktop/systemd1/unit/wayland_2dwm_40hyprland_2edesktop_2eservice";

    /// What the fake manager was asked, and whether it refuses the app's
    /// scope.
    #[derive(Default)]
    struct Asked {
        units: Mutex<Vec<String>>,
        started: Mutex<Vec<Started>>,
        cancelled: AtomicUsize,
        stopped: Mutex<Vec<(String, String)>>,
        refuse_app: AtomicBool,
    }

    /// The user manager as far as the move goes: every start queues
    /// [`JOB`], which stays queued or has ended (is not served), and the
    /// compositor's service has a main process, or is not served.
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

    /// The fake on `daemon`'s bus under the manager's name, with the job
    /// `queued` or ended and the service's `main_pid`, and a connection of
    /// the app's to the bus.
    fn serve(
        daemon: &Daemon,
        asked: &'static Asked,
        queued: bool,
        main_pid: Option<u32>,
    ) -> (Connection, Connection) {
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
        (manager.build().unwrap(), daemon.connect())
    }

    fn leak() -> &'static Asked {
        Box::leak(Box::default())
    }

    /// Nothing was called off or stopped.
    fn nothing_undone(asked: &Asked) {
        assert_eq!(asked.cancelled.load(Ordering::SeqCst), 0);
        assert!(asked.stopped.lock().unwrap().is_empty());
    }

    /// `f` on a thread of its own, failing the test when it has not
    /// returned in 10 s.
    fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (done, outcome) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(f());
        });
        outcome
            .recv_timeout(Duration::from_secs(10))
            .expect("returned within 10 s")
    }

    /// The cgroup files the fake's processes read.
    type CgroupOf = fn(u32) -> std::io::Result<String>;

    /// [`move_out`] for the app 4242 in Hyprland's unit, bounded.
    fn move_app(app: &Connection, server: Option<u32>, cgroup_of: CgroupOf) -> Outcome {
        let app = app.clone();
        bounded(move || move_out(&app, HYPRLAND, 4242, server, cgroup_of, soon()))
    }

    /// Every process once the manager moved it: the app 4242 in its scope,
    /// any other in the mount server's.
    const MOVED: CgroupOf = |pid| {
        Ok(below_user_manager(&if pid == 4242 {
            format!("app.slice/app-graphical.slice/{}", scope_name(pid))
        } else {
            format!("app.slice/{}", image_scope_name(pid))
        }))
    };

    /// Every process when the manager never moved it.
    const LEFT_BEHIND: CgroupOf = |_| Ok(below_user_manager(&format!("session.slice/{HYPRLAND}")));

    fn soon() -> Instant {
        Instant::now() + Duration::from_millis(300)
    }

    fn property(properties: &[(String, OwnedValue)], key: &str) -> OwnedValue {
        let (_, value) = properties.iter().find(|(name, _)| name == key).unwrap();
        value.try_clone().unwrap()
    }

    /// The names of the scopes started, and the `Before=` of the mount
    /// server's.
    fn starts(asked: &Asked) -> (Vec<String>, Option<Vec<String>>) {
        let started = asked.started.lock().unwrap();
        let names = started.iter().map(|(name, ..)| name.clone()).collect();
        let before = started
            .iter()
            .find(|(name, ..)| *name == image_scope_name(77))
            .map(|(_, _, properties, _)| {
                Vec::<String>::try_from(property(properties, "Before")).unwrap()
            });
        (names, before)
    }

    #[test]
    fn the_app_asks_for_a_scope_of_its_own_in_the_app_slice() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = leak();
        let (_manager, app) = serve(&daemon, asked, true, Some(7));

        let moved = move_app(&app, None, MOVED).unwrap();
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
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = leak();
        let (_manager, app) = serve(&daemon, asked, true, Some(7));

        let moved = move_app(&app, Some(77), MOVED).unwrap();
        assert_eq!(moved, Some(scope_name(4242)));
        let started = asked.started.lock().unwrap();
        let [(app_scope, ..), (name, mode, properties, auxiliary)] = started.as_slice() else {
            panic!("two starts: {}", started.len());
        };
        assert_eq!(*app_scope, scope_name(4242));
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

    /// The fake on a bus of its own, with the job `queued` or ended and
    /// the service's `main_pid`: the bus, what the fake was asked, the
    /// fake's connection and the app's.
    fn fake(
        queued: bool,
        main_pid: Option<u32>,
    ) -> Option<(Daemon, &'static Asked, Connection, Connection)> {
        let daemon = Daemon::start()?;
        let asked = leak();
        let (manager, app) = serve(&daemon, asked, queued, main_pid);
        Some((daemon, asked, manager, app))
    }

    #[test]
    fn the_services_main_process_stays() {
        let Some((_daemon, asked, _manager, app)) = fake(true, Some(4242)) else {
            return;
        };
        assert!(matches!(move_app(&app, None, MOVED), Ok(None)));
        assert!(asked.started.lock().unwrap().is_empty());
    }

    /// An app that stays where it is still moves the mount server, ordered
    /// before the unit it stays in: as its service's main process, when its
    /// service's main process is unknown, and when its own scope is refused.
    #[test]
    fn the_mount_server_leaves_whenever_the_app_stays() {
        let image = || (vec![image_scope_name(77)], Some(vec![HYPRLAND.to_owned()]));

        let Some((_daemon, asked, _manager, app)) = fake(true, Some(4242)) else {
            return;
        };
        assert!(matches!(move_app(&app, Some(77), MOVED), Ok(None)));
        assert_eq!(starts(asked), image());

        let Some((_daemon, asked, _manager, app)) = fake(true, None) else {
            return;
        };
        let error = move_app(&app, Some(77), MOVED).unwrap_err();
        assert!(matches!(error, MoveError::Bus(_)), "{error}");
        assert_eq!(starts(asked), image());

        let Some((_daemon, asked, _manager, app)) = fake(true, Some(7)) else {
            return;
        };
        asked.refuse_app.store(true, Ordering::SeqCst);
        let error = move_app(&app, Some(77), MOVED).unwrap_err();
        assert!(matches!(error, MoveError::Bus(_)), "{error}");
        assert_eq!(starts(asked), image());
        nothing_undone(asked);
    }

    /// The mount server's move counts once its cgroup names its scope; a
    /// start still queued at the deadline, a job that ended without it and
    /// no manager are errors, which `take_image` logs.
    #[test]
    fn the_mount_servers_move_is_checked() {
        let unit = "app-steno\\x2ddesktop@autostart.service";
        let image = |app: &Connection, cgroup_of: CgroupOf| {
            let app = app.clone();
            bounded(move || move_image(&app, 77, unit, cgroup_of, soon()))
        };

        let Some((_daemon, asked, _manager, app)) = fake(true, Some(7)) else {
            return;
        };
        image(&app, MOVED).unwrap();
        assert_eq!(
            starts(asked),
            (vec![image_scope_name(77)], Some(vec![unit.to_owned()]))
        );
        assert!(asked.units.lock().unwrap().is_empty());
        let error = image(&app, LEFT_BEHIND).unwrap_err();
        assert!(matches!(error, MoveError::StillQueued), "{error}");
        nothing_undone(asked);

        let Some((_daemon, _asked, _manager, app)) = fake(false, Some(7)) else {
            return;
        };
        let error = image(&app, LEFT_BEHIND).unwrap_err();
        assert!(matches!(error, MoveError::NotMoved), "{error}");
        image(&app, MOVED).unwrap();

        let Some(daemon) = Daemon::start() else {
            return;
        };
        let error = image(&daemon.connect(), MOVED).unwrap_err();
        assert!(matches!(error, MoveError::Bus(_)), "{error}");
    }

    #[test]
    fn a_service_whose_main_process_is_unknown_starts_nothing() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = leak();
        let (_manager, app) = serve(&daemon, asked, true, None);

        let error = move_app(&app, None, MOVED).unwrap_err();
        assert!(matches!(error, MoveError::Bus(_)), "{error}");
        assert!(asked.started.lock().unwrap().is_empty());
    }

    #[test]
    fn an_app_seen_in_its_scope_after_a_few_reads_is_moved() {
        static READS: AtomicUsize = AtomicUsize::new(0);
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = leak();
        let (_manager, app) = serve(&daemon, asked, true, Some(7));
        let after_three_reads: CgroupOf = |pid| {
            if READS.fetch_add(1, Ordering::SeqCst) < 3 {
                LEFT_BEHIND(pid)
            } else {
                MOVED(pid)
            }
        };

        let moved = move_app(&app, None, after_three_reads).unwrap();
        assert_eq!(moved, Some(scope_name(4242)));
        assert_eq!(READS.load(Ordering::SeqCst), 4);
        nothing_undone(asked);
    }

    /// At login the start waits behind `graphical-session.target`: past
    /// the deadline it stays queued, never called off.
    #[test]
    fn a_start_still_queued_at_the_deadline_stays_queued() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = leak();
        let (_manager, app) = serve(&daemon, asked, true, Some(7));

        let started = Instant::now();
        let error = move_app(&app, None, LEFT_BEHIND).unwrap_err();
        assert!(matches!(error, MoveError::StillQueued), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
        nothing_undone(asked);
    }

    #[test]
    fn a_job_that_ended_without_the_app_stops_nothing() {
        static JOINED: AtomicBool = AtomicBool::new(false);
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = leak();
        let (_manager, app) = serve(&daemon, asked, false, Some(7));

        let error = move_app(&app, None, LEFT_BEHIND).unwrap_err();
        assert!(matches!(error, MoveError::NotMoved), "{error}");

        // The job moved the app just before it ended.
        let joins_as_the_job_ends: CgroupOf = |pid| {
            if JOINED.swap(true, Ordering::SeqCst) {
                MOVED(pid)
            } else {
                LEFT_BEHIND(pid)
            }
        };
        let moved = move_app(&app, None, joins_as_the_job_ends).unwrap();
        assert_eq!(moved, Some(scope_name(4242)));
        nothing_undone(asked);
    }

    /// The bus answers for a manager no one runs: the name is unknown, or
    /// the session bus's activation of it fails.
    #[test]
    fn a_bus_without_a_user_manager_is_an_error() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let error = move_app(&daemon.connect(), None, MOVED).unwrap_err();
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

    /// The user bus is the socket `bus` in the runtime directory, which has
    /// to be an absolute path.
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

    /// A manager that owns its name and never answers ends the request
    /// within the call timeout of the user bus's connection.
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
        let error = move_app(&app, None, MOVED).unwrap_err();
        assert!(matches!(error, MoveError::Bus(_)), "{error}");
        assert!(started.elapsed() < CALL_TIMEOUT + Duration::from_secs(2));
    }

    /// The launch waits for the move a bounded time and goes on while the
    /// request still runs.
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

    /// An outcome that comes after the launch went on reaches the late
    /// report, once; one that comes in time never does.
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
