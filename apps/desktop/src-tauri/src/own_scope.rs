//! Linux only: the app in a systemd scope of its own when it starts inside
//! another program's service (X1 of `.plans/2026-10-07-stable-promotion.md`).
//!
//! A program that starts Steno without a unit of its own leaves it in its
//! own cgroup. On Hyprland under uwsm, a key binding's `exec` without
//! `uwsm-app` runs Steno inside the compositor's
//! `wayland-wm@hyprland.desktop.service`: stopping that unit sends SIGTERM to
//! Steno together with the compositor and `SIGKILL` ten seconds later
//! (`TimeoutStopSec=10`), while the save may take up to `SHUTDOWN_PATIENCE`
//! (ten seconds) after the request reaches it, and the process ends
//! `EXIT_GRACE` (two) after that. So at launch, before the first window
//! starts the web view's processes, the app reads `/proc/self/cgroup`. When it
//! names a service of the systemd user manager that is not Steno's own (its
//! name does not say `steno`) and whose main process is another, the app asks
//! the user manager for a transient scope that holds only itself,
//! `app-steno\x2ddesktop-<pid>.scope` in `app-graphical.slice`, with
//! `TimeoutStopSec=20s` (as `stop_timeout` gives the autostart unit and
//! GNOME's scope), `PartOf=` and `After=graphical-session.target`. The
//! session's end then stops the scope before the compositor, with the
//! display still up, and waits 20 s for the save; the web view's processes
//! and the speech sidecar start in the scope later.
//!
//! The app waits up to [`MOVE_TIMEOUT`] until its cgroup names the scope, as
//! the speech sidecar's move does (`crates/steno-speech/src/sidecar/scope.rs`):
//! a start the manager has not carried out by then is called off, and a
//! scope that started without the app is stopped. A scope, Steno's own
//! service, and an app outside the user manager (a system service, an ssh
//! login, a container, no systemd) stay where they are, silently. Every
//! other outcome is logged with the unit the app runs in afterwards, at
//! `warn` unless it moved, since the save at the session's end may be cut
//! off there. The request goes over the user bus's socket in
//! `$XDG_RUNTIME_DIR` and carries the app's pid and the scope's fixed
//! settings, nothing else.
//!
//! Swift: none; the Mac app is never in another program's unit.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// How long the app has to be in its scope before the start is called off.
const MOVE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the launch waits past [`MOVE_TIMEOUT`] for the call-off.
const CALL_OFF_TIME: Duration = Duration::from_millis(500);

/// How often the app's cgroup is read while the manager's job runs.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The scope's stop timeout: the drop-ins' 20 s (`stop_timeout`), half again
/// the save and the process's end.
const STOP_TIMEOUT: Duration = Duration::from_secs(20);

// systemd's manager: its bus name, object path and interface.
const SYSTEMD: &str = "org.freedesktop.systemd1";
const SYSTEMD_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MANAGER: &str = "org.freedesktop.systemd1.Manager";

/// Why the app is not in a scope of its own.
#[derive(Debug, thiserror::Error)]
enum MoveError {
    #[error("no user bus: XDG_RUNTIME_DIR is not an absolute path")]
    NoBus,
    #[error(transparent)]
    Bus(#[from] zbus::Error),
    #[error("the user bus did not answer in time")]
    Silent,
    #[error("the user manager had not moved the app in time; the start was called off")]
    CalledOff,
    #[error("the user manager started the scope without the app; its stop was asked for")]
    NotMoved,
    #[error("the request's thread did not start: {0}")]
    Thread(std::io::Error),
}

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
    let mut parts = unified(cgroup)?.rsplit('/');
    let unit = parts.next()?;
    let in_user_manager = parts.any(|part| part.starts_with("user@") && part.ends_with(".service"));
    let named = unit
        .strip_suffix(".service")
        .is_some_and(|stem| !stem.is_empty() && !stem.starts_with('_'));
    (in_user_manager && named && !unit.to_ascii_lowercase().contains("steno")).then_some(unit)
}

/// The app's scope, named as the XDG convention for applications' units has
/// it (`app-<id>-<random>.scope`), the pid as the random part.
fn scope_name(pid: u32) -> String {
    format!("app-steno\\x2ddesktop-{pid}.scope")
}

/// What the scope is started with.
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
        "org.freedesktop.DBus.Properties",
        "Get",
        &("org.freedesktop.systemd1.Service", "MainPID"),
    )?;
    Ok(u32::try_from(pid)?)
}

/// Moves the app `pid`, which runs in another program's service `unit`, into a
/// scope of its own over `connection`; the scope's name once `read_cgroup`
/// names it, or `None` when the app is `unit`'s main process and stays.
///
/// `StartTransientUnit` answers once the manager queued the job, and the job
/// moves the app later. An app not in its scope by `deadline` has the start
/// called off: a job the manager cancels never ran. A job whose cancel the
/// manager answers with an error has run: an app in its scope then stays
/// there, and a scope without it is stopped.
fn move_out(
    connection: &Connection,
    unit: &str,
    pid: u32,
    read_cgroup: impl Fn() -> std::io::Result<String>,
    deadline: Instant,
) -> Result<Option<String>, MoveError> {
    if main_pid(connection, unit)? == pid {
        return Ok(None);
    }
    let name = scope_name(pid);
    let auxiliary: Vec<(&str, Vec<(&str, Value)>)> = Vec::new();
    let job: OwnedObjectPath = call(
        connection,
        SYSTEMD_PATH,
        SYSTEMD_MANAGER,
        "StartTransientUnit",
        &(name.as_str(), "fail", properties(pid), auxiliary),
    )?;
    let in_scope = || read_cgroup().is_ok_and(|cgroup| innermost(&cgroup) == Some(name.as_str()));
    while !in_scope() {
        if Instant::now() >= deadline {
            let cancelled: zbus::Result<()> = call(
                connection,
                job.as_str(),
                "org.freedesktop.systemd1.Job",
                "Cancel",
                &(),
            );
            return match cancelled {
                Ok(()) => Err(MoveError::CalledOff),
                Err(zbus::Error::MethodError(..)) if in_scope() => Ok(Some(name)),
                Err(zbus::Error::MethodError(..)) => {
                    let _: zbus::Result<OwnedObjectPath> = call(
                        connection,
                        SYSTEMD_PATH,
                        SYSTEMD_MANAGER,
                        "StopUnit",
                        &(name.as_str(), "replace"),
                    );
                    Err(MoveError::NotMoved)
                }
                Err(error) => Err(error.into()),
            };
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    Ok(Some(name))
}

/// This process's cgroup file.
fn own_cgroup() -> std::io::Result<String> {
    std::fs::read_to_string("/proc/self/cgroup")
}

/// Moves the app into a scope of its own when it runs in another program's
/// service (the module docs), waiting at most [`MOVE_TIMEOUT`] and the
/// call-off. Called at launch, before the first window. Never fails: the app
/// records where it is otherwise.
pub fn leave_foreign_service() {
    let Some(unit) = own_cgroup()
        .ok()
        .as_deref()
        .and_then(foreign_service)
        .map(str::to_owned)
    else {
        return;
    };
    let pid = std::process::id();
    let deadline = Instant::now() + MOVE_TIMEOUT;
    let request = unit.clone();
    let (done, outcome) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("steno-own-scope".to_owned())
        .spawn(move || {
            let moved = (|| {
                let runtime = std::env::var_os("XDG_RUNTIME_DIR")
                    .map(PathBuf::from)
                    .filter(|dir| dir.is_absolute())
                    .ok_or(MoveError::NoBus)?;
                let address = format!("unix:path={}", runtime.join("bus").display());
                let connection = Builder::address(address.as_str())?
                    .method_timeout(MOVE_TIMEOUT)
                    .build()?;
                move_out(&connection, &request, pid, own_cgroup, deadline)
            })();
            let _ = done.send(moved);
        });
    let moved = match spawned {
        Ok(_) => outcome
            .recv_timeout(MOVE_TIMEOUT + CALL_OFF_TIME)
            .unwrap_or(Err(MoveError::Silent)),
        Err(error) => Err(MoveError::Thread(error)),
    };
    let now = own_cgroup()
        .ok()
        .as_deref()
        .and_then(innermost)
        .map(str::to_owned);
    let now = now.as_deref().unwrap_or("unknown");
    match moved {
        Ok(Some(scope)) => tracing::info!(
            from = %unit,
            %scope,
            "Steno started inside another program's service and moved into a scope of its own, which gives a save at the session's end 20 s"
        ),
        Ok(None) => tracing::debug!(%unit, "Steno is its service's main process and stays in it"),
        Err(error) => tracing::warn!(
            %unit,
            %error,
            now,
            "Steno runs inside another program's service, whose stop may cut off the save of a recording; start Steno from the app launcher or with `uwsm-app -- steno-desktop`"
        ),
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
        ] {
            assert_eq!(foreign_service(&stays), None, "{stays}");
        }
    }

    #[test]
    fn the_scope_outlasts_the_save_and_ends_with_the_graphical_session() {
        assert_eq!(scope_name(4242), "app-steno\\x2ddesktop-4242.scope");
        assert_eq!(
            innermost(&below_user_manager("app.slice/x.scope")),
            Some("x.scope")
        );
        let save = steno_services::app::SHUTDOWN_PATIENCE + crate::EXIT_GRACE;
        assert!(STOP_TIMEOUT >= save * 3 / 2);
        assert!(MOVE_TIMEOUT + CALL_OFF_TIME <= Duration::from_secs(3));
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

    /// What the fake manager was asked.
    #[derive(Default)]
    struct Asked {
        units: Mutex<Vec<String>>,
        started: Mutex<Vec<Started>>,
        cancelled: AtomicUsize,
        stopped: Mutex<Vec<(String, String)>>,
    }

    /// The user manager as far as the move goes: its job answers `Cancel`
    /// as queued (`ran` false) or as run (`ran` true, an error), and the
    /// compositor's service has `main_pid`.
    struct FakeManager(&'static Asked);
    struct FakeJob(&'static Asked, bool);
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
        ) -> OwnedObjectPath {
            self.0
                .started
                .lock()
                .unwrap()
                .push((name, mode, properties, auxiliary));
            OwnedObjectPath::try_from(JOB).unwrap()
        }

        fn stop_unit(&self, name: String, mode: String) -> OwnedObjectPath {
            self.0.stopped.lock().unwrap().push((name, mode));
            OwnedObjectPath::try_from("/org/freedesktop/systemd1/job/2").unwrap()
        }
    }

    #[zbus::interface(name = "org.freedesktop.systemd1.Job")]
    impl FakeJob {
        fn cancel(&self) -> zbus::fdo::Result<()> {
            self.0.cancelled.fetch_add(1, Ordering::SeqCst);
            if self.1 {
                Err(zbus::fdo::Error::UnknownObject(format!(
                    "Unknown object '{JOB}'."
                )))
            } else {
                Ok(())
            }
        }
    }

    #[zbus::interface(name = "org.freedesktop.systemd1.Service")]
    impl FakeService {
        #[zbus(property, name = "MainPID")]
        fn main_pid(&self) -> u32 {
            self.0
        }
    }

    /// The fake on `daemon`'s bus under the manager's name, and a connection
    /// of the app's to the bus.
    fn serve(
        daemon: &Daemon,
        asked: &'static Asked,
        ran: bool,
        main_pid: u32,
    ) -> (Connection, Connection) {
        let manager = daemon
            .builder()
            .name(SYSTEMD)
            .unwrap()
            .serve_at(SYSTEMD_PATH, FakeManager(asked))
            .unwrap()
            .serve_at(JOB, FakeJob(asked, ran))
            .unwrap()
            .serve_at(UNIT, FakeService(main_pid))
            .unwrap()
            .build()
            .unwrap();
        (manager, daemon.connect())
    }

    fn leak() -> &'static Asked {
        Box::leak(Box::default())
    }

    /// The app's cgroup once the manager moved it.
    const IN_SCOPE: fn() -> std::io::Result<String> = || {
        Ok(below_user_manager(&format!(
            "app.slice/app-graphical.slice/{}",
            scope_name(4242)
        )))
    };

    /// The app's cgroup when the manager never moved it.
    const LEFT_BEHIND: fn() -> std::io::Result<String> =
        || Ok(below_user_manager(&format!("session.slice/{HYPRLAND}")));

    fn soon() -> Instant {
        Instant::now() + Duration::from_millis(300)
    }

    fn property(properties: &[(String, OwnedValue)], key: &str) -> OwnedValue {
        let (_, value) = properties.iter().find(|(name, _)| name == key).unwrap();
        value.try_clone().unwrap()
    }

    #[test]
    fn the_app_asks_for_a_scope_of_its_own_in_the_app_slice() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = leak();
        let (_manager, app) = serve(&daemon, asked, false, 7);

        let moved = move_out(&app, HYPRLAND, 4242, IN_SCOPE, soon()).unwrap();
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
        assert_eq!(asked.cancelled.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn the_services_main_process_stays() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = leak();
        let (_manager, app) = serve(&daemon, asked, false, 4242);

        assert!(matches!(
            move_out(&app, HYPRLAND, 4242, IN_SCOPE, soon()),
            Ok(None)
        ));
        assert!(asked.started.lock().unwrap().is_empty());
    }

    #[test]
    fn an_app_seen_in_its_scope_after_a_few_reads_is_moved() {
        static READS: AtomicUsize = AtomicUsize::new(0);
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = leak();
        let (_manager, app) = serve(&daemon, asked, false, 7);
        let after_three_reads = || {
            if READS.fetch_add(1, Ordering::SeqCst) < 3 {
                LEFT_BEHIND()
            } else {
                IN_SCOPE()
            }
        };

        let moved = move_out(&app, HYPRLAND, 4242, after_three_reads, soon()).unwrap();
        assert_eq!(moved, Some(scope_name(4242)));
        assert_eq!(READS.load(Ordering::SeqCst), 4);
        assert_eq!(asked.cancelled.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn an_app_not_moved_in_time_has_the_start_called_off() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = leak();
        let (_manager, app) = serve(&daemon, asked, false, 7);

        let error = move_out(&app, HYPRLAND, 4242, LEFT_BEHIND, soon()).unwrap_err();
        assert!(matches!(error, MoveError::CalledOff), "{error}");
        assert_eq!(asked.cancelled.load(Ordering::SeqCst), 1);
        assert!(asked.stopped.lock().unwrap().is_empty());
    }

    #[test]
    fn a_job_that_ran_keeps_the_app_moved_and_stops_an_empty_scope() {
        static JOINED: AtomicBool = AtomicBool::new(false);
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = leak();
        let (_manager, app) = serve(&daemon, asked, true, 7);

        let error = move_out(&app, HYPRLAND, 4242, LEFT_BEHIND, soon()).unwrap_err();
        assert!(matches!(error, MoveError::NotMoved), "{error}");
        assert_eq!(
            *asked.stopped.lock().unwrap(),
            [(scope_name(4242), "replace".to_owned())]
        );

        // The job moved the app just as the deadline passed.
        let joins_at_the_call_off = || {
            if JOINED.swap(true, Ordering::SeqCst) {
                IN_SCOPE()
            } else {
                LEFT_BEHIND()
            }
        };
        let moved = move_out(&app, HYPRLAND, 4242, joins_at_the_call_off, Instant::now()).unwrap();
        assert_eq!(moved, Some(scope_name(4242)));
        assert_eq!(asked.stopped.lock().unwrap().len(), 1);
    }

    /// The bus answers for a manager no one runs: the name is unknown, or
    /// the session bus's activation of it fails.
    #[test]
    fn a_bus_without_a_user_manager_is_an_error() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let error = move_out(&daemon.connect(), HYPRLAND, 4242, IN_SCOPE, soon()).unwrap_err();
        assert!(
            matches!(&error, MoveError::Bus(zbus::Error::MethodError(name, ..)) if name.starts_with("org.freedesktop.DBus.Error.")),
            "{error}"
        );
    }
}
