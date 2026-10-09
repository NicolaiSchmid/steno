//! Linux only: the child in a systemd scope of its own (P6 of
//! `.plans/2026-10-07-stable-promotion.md`).
//!
//! A desktop launched by a systemd user session runs in a unit of the
//! user manager, a scope or a service under `app.slice`, and its child
//! joins that unit's cgroup. `systemd-oomd`, which some distributions turn
//! on for the cgroups of the user's session
//! (`ManagedOOMMemoryPressure=kill`), kills a whole cgroup: under memory
//! pressure from the child working through a long meeting, it would kill
//! the app with it, and the recording in progress. So right after the
//! spawn the client asks the user manager to start a transient scope
//! beside the app's unit, in the same slice, holding only the child, and
//! waits until the child's cgroup names that scope. `systemd-oomd` then
//! weighs the two cgroups apart and takes the child's first; the client
//! sees a dead child as after any crash, and the next call starts
//! another, in a scope of its own. The app's own cgroup is still a
//! candidate: speaker diarization runs in the app's process
//! (`crates/steno-diarize/src/onnx.rs`), and pressure that lasts after the
//! child is gone can take the app too.
//!
//! The scope is `PartOf` the app's unit, so stopping that unit stops the
//! child too; the child also ends at the app's exit, as everywhere (the
//! `steno-speech-sidecar` crate docs). A kernel OOM kill of the child no
//! longer counts against the app's unit either. An app outside a user unit
//! (a system service, an ssh login, a container without a user manager, a
//! distribution without systemd) keeps its child in its own cgroup, as
//! does any failure here, which is logged at info: the app records either
//! way.
//!
//! The request goes over the user bus's Unix socket in
//! `$XDG_RUNTIME_DIR`, where the user manager listens, and carries the
//! child's pid and unit names, nothing else. The child itself still opens
//! nothing.
//!
//! - [`placement`]: the app's unit and slice from its cgroup.
//! - [`move_within`]: the request on a thread of its own, waited for a
//!   bounded time, one at a time.
//! - [`move_to_own_scope`]: the one entry point, called by `client` right
//!   after the spawn.
//!
//! Swift: none; the Mac app runs `FluidAudio` in-process only.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::zvariant::Value;

/// How long the spawn waits for the child's scope.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(2);

/// How often the child's cgroup is read while the manager's job runs.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Set while a request is under way, so a bus that never answers holds
/// one thread and one socket at most, not one per spawn.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// What the move reads from the system; the tests supply their own.
#[derive(Debug, Clone)]
struct Host {
    /// The app's `/proc/self/cgroup`.
    cgroup: String,
    /// `$XDG_RUNTIME_DIR`, where the user bus listens.
    runtime_dir: PathBuf,
    /// The child's cgroup file, by pid.
    child_cgroup: fn(u32) -> std::io::Result<String>,
}

impl Host {
    /// This process's: `None` without a cgroup file or an absolute
    /// `$XDG_RUNTIME_DIR`.
    fn current() -> Option<Self> {
        let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())?;
        Some(Self {
            cgroup,
            runtime_dir,
            child_cgroup: proc_cgroup,
        })
    }
}

/// Stands in for [`Host::current`] while set, for the spawn's test.
#[cfg(test)]
static TEST_HOST: std::sync::Mutex<Option<Host>> = std::sync::Mutex::new(None);

/// The cgroup file of process `pid`.
fn proc_cgroup(pid: u32) -> std::io::Result<String> {
    std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
}

/// Why the child stays in the app's cgroup.
#[derive(Debug, thiserror::Error)]
enum ScopeError {
    #[error("no user bus: {0}")]
    NoBus(std::io::Error),
    #[error(transparent)]
    Bus(#[from] zbus::Error),
    #[error("the user bus took the connection too late")]
    Late,
    #[error("the user bus did not answer in time")]
    Silent,
    #[error("the user manager did not move the child into its scope in time")]
    NotMoved,
    #[error("an earlier request to the user bus is still waiting")]
    Busy,
    #[error("the request's thread did not start: {0}")]
    Thread(std::io::Error),
}

/// Where the app runs: its unit and the slice that holds it, as named in
/// its cgroup's path.
#[derive(Debug, PartialEq, Eq)]
struct Placement {
    unit: String,
    slice: String,
}

/// The cgroup path of a cgroup file's unified (`0::`) line.
fn unified(cgroup: &str) -> Option<&str> {
    cgroup.lines().find_map(|line| line.strip_prefix("0::"))
}

/// The app's unit and slice from `/proc/self/cgroup`, when the app runs
/// in a scope or service of a systemd user manager (a path through
/// `user@<uid>.service`) directly under a slice; `None` otherwise, and on
/// cgroup v1, which has no `0::` line.
fn placement(cgroup: &str) -> Option<Placement> {
    let mut parts = unified(cgroup)?.rsplit('/');
    let unit = parts.next()?;
    let slice = parts.next()?;
    let in_user_manager =
        parts.any(|part| part.starts_with("user@") && kind(part) == Some("service"));
    (in_user_manager
        && kind(slice) == Some("slice")
        && matches!(kind(unit), Some("scope" | "service")))
    .then(|| Placement {
        unit: unit.to_owned(),
        slice: slice.to_owned(),
    })
}

/// A unit's type, the part of its name after the last dot. A name that
/// starts with `_` is one systemd escaped for the cgroup tree, not the
/// unit's, and has none.
fn kind(name: &str) -> Option<&str> {
    let (stem, kind) = name.rsplit_once('.')?;
    (!stem.is_empty() && !name.starts_with('_')).then_some(kind)
}

/// Whether the cgroup file `cgroup` puts its process in the unit `scope`.
fn holds(cgroup: &str, scope: &str) -> bool {
    unified(cgroup).and_then(|path| path.rsplit('/').next()) == Some(scope)
}

/// The child's scope, named as the XDG convention for applications'
/// units has it (`app-<id>-<random>.scope`), the pid as the random part.
fn scope_name(pid: u32) -> String {
    format!("app-steno\\x2dspeech\\x2dsidecar-{pid}.scope")
}

/// What the scope is started with.
fn properties(pid: u32, placement: &Placement) -> Vec<(&'static str, Value<'_>)> {
    vec![
        ("Description", Value::from("Steno speech sidecar")),
        ("PIDs", Value::from(vec![pid])),
        ("Slice", Value::from(placement.slice.as_str())),
        ("PartOf", Value::from(vec![placement.unit.as_str()])),
        // A scope `systemd-oomd` killed ends failed; it is collected
        // then, not kept for `systemctl --user --failed`.
        ("CollectMode", Value::from("inactive-or-failed")),
    ]
}

/// Calls the user manager's `method` on `connection`.
fn manager<B>(connection: &Connection, method: &str, body: &B) -> zbus::Result<()>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    connection.call_method(
        Some("org.freedesktop.systemd1"),
        "/org/freedesktop/systemd1",
        Some("org.freedesktop.systemd1.Manager"),
        method,
        body,
    )?;
    Ok(())
}

/// Moves the child `pid` into a scope of its own beside the app's unit
/// at `placement`, over the user bus in `runtime_dir`; the scope's name
/// once `child_cgroup` names it. Every call waits at most the time left
/// before `deadline` when the connection started. A bus that has not
/// taken the connection by `deadline` is not asked: the spawn stopped
/// waiting then, and a child that died since may be reaped and its pid
/// reused.
///
/// `StartTransientUnit` answers once the manager queued the job, and the
/// job moves the child later. A child not in its scope by `deadline`
/// (it died first, or the manager is slow) has the scope stopped, so an
/// empty one is not left active and a job still queued is cancelled.
fn start_scope_over_bus(
    pid: u32,
    placement: &Placement,
    runtime_dir: &Path,
    child_cgroup: fn(u32) -> std::io::Result<String>,
    deadline: Instant,
) -> Result<String, ScopeError> {
    let stream = UnixStream::connect(runtime_dir.join("bus")).map_err(ScopeError::NoBus)?;
    let connection = Builder::async_io_unix_stream(stream)
        .method_timeout(deadline.saturating_duration_since(Instant::now()))
        .build()?;
    if Instant::now() >= deadline {
        return Err(ScopeError::Late);
    }
    let name = scope_name(pid);
    let auxiliary: Vec<(&str, Vec<(&str, Value)>)> = Vec::new();
    manager(
        &connection,
        "StartTransientUnit",
        &(name.as_str(), "fail", properties(pid, placement), auxiliary),
    )?;
    while !child_cgroup(pid).is_ok_and(|cgroup| holds(&cgroup, &name)) {
        if Instant::now() >= deadline {
            if let Err(error) = manager(&connection, "StopUnit", &(name.as_str(), "replace")) {
                tracing::info!(pid, %error, scope = %name, "the speech sidecar's scope could not be stopped");
            }
            return Err(ScopeError::NotMoved);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    Ok(name)
}

/// Clears the in-flight flag when the request's thread ends, panics
/// included.
struct InFlight(&'static AtomicBool);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// [`start_scope_over_bus`] on a thread of its own, waited for `timeout`
/// at most, when `host` places the app in a user unit; `None` when it
/// does not and nothing was asked. The method timeout does not cover the
/// connection's set-up, so a bus that accepts and never answers would
/// otherwise hold the spawn. A thread still waiting then runs on until
/// the bus answers or closes, and logs its outcome itself; while it does,
/// `in_flight` is set and later spawns do not ask.
fn move_within(
    pid: u32,
    host: &Host,
    timeout: Duration,
    in_flight: &'static AtomicBool,
) -> Result<Option<String>, ScopeError> {
    let Some(placement) = placement(&host.cgroup) else {
        return Ok(None);
    };
    if in_flight.swap(true, Ordering::SeqCst) {
        return Err(ScopeError::Busy);
    }
    let guard = InFlight(in_flight);
    let deadline = Instant::now() + timeout;
    let (done, outcome) = mpsc::channel();
    let runtime_dir = host.runtime_dir.clone();
    let child_cgroup = host.child_cgroup;
    std::thread::Builder::new()
        .name(format!("sidecar-{pid}-scope"))
        .spawn(move || {
            let outcome =
                start_scope_over_bus(pid, &placement, &runtime_dir, child_cgroup, deadline);
            drop(guard);
            if let Err(mpsc::SendError(late)) = done.send(outcome) {
                match late {
                    Ok(scope) => tracing::info!(
                        pid,
                        %scope,
                        "speech sidecar in a scope of its own, after the spawn stopped waiting"
                    ),
                    Err(error) => tracing::info!(
                        pid,
                        %error,
                        "the speech sidecar stays in the app's cgroup: its scope could not be started"
                    ),
                }
            }
        })
        .map_err(ScopeError::Thread)?;
    outcome
        .recv_timeout(timeout)
        .unwrap_or(Err(ScopeError::Silent))
        .map(Some)
}

/// Moves the just-spawned child `pid` into a scope of its own when the
/// app runs in a systemd user unit (the module docs), waiting
/// [`ANSWER_TIMEOUT`] at most. Called before the parent waits on the
/// child, so `pid` cannot name another process yet. Never fails: the child
/// stays in the app's cgroup otherwise.
pub(super) fn move_to_own_scope(pid: u32) {
    #[cfg(test)]
    let host = TEST_HOST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .or_else(Host::current);
    #[cfg(not(test))]
    let host = Host::current();
    let Some(host) = host else {
        return;
    };
    match move_within(pid, &host, ANSWER_TIMEOUT, &IN_FLIGHT) {
        Ok(Some(scope)) => tracing::info!(pid, %scope, "speech sidecar in a scope of its own"),
        Ok(None) => tracing::debug!(
            pid,
            "the app runs in no systemd user unit; the speech sidecar shares its cgroup"
        ),
        Err(ScopeError::Silent) => tracing::info!(
            pid,
            "the user bus did not answer in time; the speech sidecar is in the app's cgroup until its scope starts"
        ),
        Err(error) => {
            tracing::info!(pid, %error, "the speech sidecar shares the app's cgroup: its scope could not be started");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::BufRead as _;
    use std::os::unix::net::UnixListener;

    use steno_core::SpeechEngine as _;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue};

    use super::*;
    use crate::{ModelStore, SidecarConfig, SidecarSpeechEngine};

    const UWSM: &str = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-graphical.slice/app-Hyprland-steno\\x2ddesktop-1234.scope\n";

    /// The child's cgroup once the manager moved it.
    const IN_ITS_SCOPE: fn(u32) -> std::io::Result<String> = |pid| {
        Ok(format!(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-graphical.slice/{}\n",
            scope_name(pid)
        ))
    };

    /// The child's cgroup when the manager never moved it.
    const LEFT_BEHIND: fn(u32) -> std::io::Result<String> = |_| Ok(UWSM.to_owned());

    fn host(runtime_dir: &Path, child_cgroup: fn(u32) -> std::io::Result<String>) -> Host {
        Host {
            cgroup: UWSM.to_owned(),
            runtime_dir: runtime_dir.to_owned(),
            child_cgroup,
        }
    }

    #[test]
    fn the_unit_and_slice_come_from_a_user_managers_cgroup_only() {
        assert_eq!(
            placement(UWSM),
            Some(Placement {
                unit: "app-Hyprland-steno\\x2ddesktop-1234.scope".to_owned(),
                slice: "app-graphical.slice".to_owned(),
            })
        );
        assert_eq!(
            placement(
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-steno\\x2ddesktop@autostart.service"
            ),
            Some(Placement {
                unit: "app-steno\\x2ddesktop@autostart.service".to_owned(),
                slice: "app.slice".to_owned(),
            })
        );
        // A hybrid system's v1 lines are skipped for the unified one.
        assert!(placement(&format!("12:pids:/user.slice\n1:name=systemd:/x\n{UWSM}")).is_some());
        for outside in [
            // A system service, an ssh login, the user manager itself.
            "0::/system.slice/runner.service",
            "0::/user.slice/user-1000.slice/session-3.scope",
            "0::/user.slice/user-1000.slice/user@1000.service/init.scope",
            // Units below a system service that delegates its subtree.
            "0::/system.slice/runner.service/app.slice/job.scope",
            // A scope below a delegated service, not a slice; a `user@`
            // that is a slice, not the manager's service.
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/x.service/y.scope",
            "0::/user.slice/user@1000.slice/app.slice/a.scope",
            // A sub-cgroup inside a delegated unit, an escaped name, a
            // name with nothing before its type.
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/vte.scope/tab-1",
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/_cgroup.scope",
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/.scope",
            // A container's root, cgroup v1 only, nothing.
            "0::/",
            "1:name=systemd:/user.slice/user-1000.slice/user@1000.service/app.slice/a.scope",
            "",
        ] {
            assert_eq!(placement(outside), None, "{outside}");
        }
    }

    #[test]
    fn the_spawn_waits_two_seconds_at_most_and_reads_this_process() {
        assert!(ANSWER_TIMEOUT <= Duration::from_secs(2));
        let own = std::fs::read_to_string("/proc/self/cgroup").unwrap();
        assert_eq!(proc_cgroup(std::process::id()).unwrap(), own);
        if let Some(host) = Host::current() {
            assert_eq!(host.cgroup, own);
            assert!(host.runtime_dir.is_absolute());
        }
        assert!(holds(&IN_ITS_SCOPE(7).unwrap(), &scope_name(7)));
        assert!(!holds(UWSM, &scope_name(7)));
    }

    /// A private bus: `dbus-daemon` on a socket named `bus` in a
    /// directory of its own, as the user bus is in `$XDG_RUNTIME_DIR`,
    /// with no service directories, so it starts nothing for a name no
    /// one owns.
    struct Daemon {
        child: std::process::Child,
        dir: tempfile::TempDir,
    }

    impl Daemon {
        /// None when `dbus-daemon` is not installed, unless
        /// `STENO_REQUIRE_DBUS_TEST` asks for it (CI on Linux).
        fn start() -> Option<Self> {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("bus.conf");
            std::fs::write(
                &config,
                format!(
                    "<busconfig><type>session</type><listen>unix:path={}</listen>\
                     <policy context=\"default\"><allow send_destination=\"*\"/>\
                     <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
                    dir.path().join("bus").display()
                ),
            )
            .unwrap();
            let spawned = std::process::Command::new("dbus-daemon")
                .args(["--nofork", "--nopidfile", "--print-address=1"])
                .arg(format!("--config-file={}", config.display()))
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
            // The address line says the socket is listening.
            let mut address = String::new();
            std::io::BufReader::new(child.stdout.take().expect("piped"))
                .read_line(&mut address)
                .expect("the daemon's address");
            Some(Self { child, dir })
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
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

    /// The user manager as far as a transient unit goes.
    struct FakeManager {
        started: mpsc::Sender<Started>,
        stopped: mpsc::Sender<(String, String)>,
    }

    /// What the fake manager was asked, in order, per method.
    struct Asked {
        started: mpsc::Receiver<Started>,
        stopped: mpsc::Receiver<(String, String)>,
        _connection: Connection,
    }

    impl FakeManager {
        /// The fake on `daemon`'s bus under the manager's name.
        fn serve(daemon: &Daemon) -> Asked {
            let (started, started_calls) = mpsc::channel();
            let (stopped, stopped_calls) = mpsc::channel();
            let address = format!("unix:path={}", daemon.path().join("bus").display());
            let connection = Builder::address(address.as_str())
                .unwrap()
                .name("org.freedesktop.systemd1")
                .unwrap()
                .serve_at("/org/freedesktop/systemd1", Self { started, stopped })
                .unwrap()
                .build()
                .unwrap();
            Asked {
                started: started_calls,
                stopped: stopped_calls,
                _connection: connection,
            }
        }
    }

    #[zbus::interface(name = "org.freedesktop.systemd1.Manager")]
    impl FakeManager {
        fn start_transient_unit(
            &self,
            name: String,
            mode: String,
            properties: Vec<(String, OwnedValue)>,
            auxiliary: Vec<(String, Vec<(String, OwnedValue)>)>,
        ) -> OwnedObjectPath {
            let _ = self.started.send((name, mode, properties, auxiliary));
            OwnedObjectPath::try_from("/org/freedesktop/systemd1/job/1").unwrap()
        }

        fn stop_unit(&self, name: String, mode: String) -> OwnedObjectPath {
            let _ = self.stopped.send((name, mode));
            OwnedObjectPath::try_from("/org/freedesktop/systemd1/job/2").unwrap()
        }
    }

    /// The value of property `key` in a `StartTransientUnit` call.
    fn property(properties: &[(String, OwnedValue)], key: &str) -> OwnedValue {
        let (_, value) = properties.iter().find(|(name, _)| name == key).unwrap();
        value.try_clone().unwrap()
    }

    /// Waits until the request's thread cleared `in_flight`.
    fn ended(in_flight: &AtomicBool) {
        let asked = Instant::now();
        while in_flight.load(Ordering::SeqCst) {
            assert!(asked.elapsed() < Duration::from_secs(10), "still waiting");
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    #[test]
    fn the_child_gets_a_scope_beside_the_apps_unit_and_part_of_it() {
        static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = FakeManager::serve(&daemon);

        let scope = move_within(
            4242,
            &host(daemon.path(), IN_ITS_SCOPE),
            ANSWER_TIMEOUT,
            &IN_FLIGHT,
        )
        .unwrap();
        assert_eq!(
            scope.as_deref(),
            Some("app-steno\\x2dspeech\\x2dsidecar-4242.scope")
        );
        let (name, mode, properties, auxiliary) = asked.started.try_recv().expect("one call");
        assert!(auxiliary.is_empty());
        assert_eq!(name, "app-steno\\x2dspeech\\x2dsidecar-4242.scope");
        assert_eq!(mode, "fail");
        assert_eq!(
            Vec::<u32>::try_from(property(&properties, "PIDs")).unwrap(),
            [4242]
        );
        assert_eq!(
            String::try_from(property(&properties, "Slice")).unwrap(),
            "app-graphical.slice"
        );
        assert_eq!(
            Vec::<String>::try_from(property(&properties, "PartOf")).unwrap(),
            ["app-Hyprland-steno\\x2ddesktop-1234.scope"]
        );
        assert_eq!(
            String::try_from(property(&properties, "CollectMode")).unwrap(),
            "inactive-or-failed"
        );
        assert_eq!(properties.len(), 5);
        assert!(asked.stopped.try_recv().is_err());
        assert!(!IN_FLIGHT.load(Ordering::SeqCst));
    }

    #[test]
    fn a_child_the_manager_does_not_move_in_time_has_its_scope_stopped() {
        static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = FakeManager::serve(&daemon);

        let timeout = Duration::from_millis(300);
        let error =
            move_within(4242, &host(daemon.path(), LEFT_BEHIND), timeout, &IN_FLIGHT).unwrap_err();
        // The thread stops the scope at the deadline; the spawn may have
        // stopped waiting a moment earlier.
        assert!(
            matches!(error, ScopeError::NotMoved | ScopeError::Silent),
            "{error}"
        );
        ended(&IN_FLIGHT);
        assert!(asked.started.try_recv().is_ok());
        assert_eq!(
            asked.stopped.try_recv().expect("the scope stopped"),
            (scope_name(4242), "replace".to_owned())
        );
    }

    #[test]
    fn outside_a_user_unit_nothing_is_asked() {
        static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
        let outside = Host {
            cgroup: "0::/system.slice/steno.service".to_owned(),
            ..host(Path::new("/nonexistent"), IN_ITS_SCOPE)
        };
        assert!(matches!(
            move_within(4242, &outside, ANSWER_TIMEOUT, &IN_FLIGHT),
            Ok(None)
        ));
        assert!(!IN_FLIGHT.load(Ordering::SeqCst));
    }

    /// A socket at `bus` in a directory of its own that forwards one
    /// connection to `daemon`'s, holding back the daemon's side by `delay`.
    fn slow_bus(daemon: &Path, delay: Duration) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(dir.path().join("bus")).unwrap();
        let daemon = daemon.join("bus");
        std::thread::spawn(move || {
            let (mut client, _) = listener.accept().unwrap();
            let mut upstream = UnixStream::connect(daemon).unwrap();
            let (mut to_daemon, mut from_client) =
                (upstream.try_clone().unwrap(), client.try_clone().unwrap());
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut from_client, &mut to_daemon);
                let _ = to_daemon.shutdown(std::net::Shutdown::Write);
            });
            std::thread::sleep(delay);
            let _ = std::io::copy(&mut upstream, &mut client);
        });
        dir
    }

    #[test]
    fn every_unanswered_request_is_an_error_to_log() {
        static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
        let in_time = || Instant::now() + ANSWER_TIMEOUT;
        let uwsm = placement(UWSM).unwrap();
        let nowhere = Path::new("/nonexistent");
        assert!(matches!(
            start_scope_over_bus(1, &uwsm, nowhere, IN_ITS_SCOPE, in_time()),
            Err(ScopeError::NoBus(_))
        ));

        // One request at a time: a later spawn does not ask.
        IN_FLIGHT.store(true, Ordering::SeqCst);
        assert!(matches!(
            move_within(1, &host(nowhere, IN_ITS_SCOPE), ANSWER_TIMEOUT, &IN_FLIGHT),
            Err(ScopeError::Busy)
        ));
        IN_FLIGHT.store(false, Ordering::SeqCst);

        // A socket that takes the connection and never answers: the wait
        // ends at its timeout, and once the socket closes the thread ends.
        let silent = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(silent.path().join("bus")).unwrap();
        let asked = Instant::now();
        let error = move_within(
            1,
            &host(silent.path(), IN_ITS_SCOPE),
            Duration::from_millis(200),
            &IN_FLIGHT,
        )
        .unwrap_err();
        assert!(
            asked.elapsed() < Duration::from_secs(2),
            "{:?}",
            asked.elapsed()
        );
        assert!(matches!(error, ScopeError::Silent), "{error}");
        assert!(IN_FLIGHT.load(Ordering::SeqCst));
        drop(listener);
        ended(&IN_FLIGHT);

        let Some(daemon) = Daemon::start() else {
            return;
        };
        let error =
            start_scope_over_bus(1, &uwsm, daemon.path(), IN_ITS_SCOPE, in_time()).unwrap_err();
        assert!(
            matches!(&error, ScopeError::Bus(zbus::Error::MethodError(name, ..)) if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown"),
            "{error}"
        );

        // A bus that takes the connection after the deadline is not
        // asked, whether the deadline passed before the connection or
        // during its set-up.
        let asked = FakeManager::serve(&daemon);
        assert!(matches!(
            start_scope_over_bus(1, &uwsm, daemon.path(), IN_ITS_SCOPE, Instant::now()),
            Err(ScopeError::Late)
        ));
        let slow = slow_bus(daemon.path(), Duration::from_millis(500));
        let error = move_within(
            1,
            &host(slow.path(), IN_ITS_SCOPE),
            Duration::from_millis(200),
            &IN_FLIGHT,
        )
        .unwrap_err();
        assert!(matches!(error, ScopeError::Silent), "{error}");
        ended(&IN_FLIGHT);
        assert!(asked.started.try_recv().is_err());
    }

    /// The spawn itself asks for the child's scope: `sh` stands in for
    /// the sidecar, exits at once and never says it is ready.
    #[tokio::test]
    async fn the_spawn_moves_its_child() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = FakeManager::serve(&daemon);
        *TEST_HOST.lock().unwrap() = Some(host(daemon.path(), IN_ITS_SCOPE));
        let dir = tempfile::tempdir().unwrap();
        let mut config = SidecarConfig::new("/bin/sh");
        config.args = vec!["-c".into(), "exit 0".into()];
        let engine =
            SidecarSpeechEngine::with_assets(ModelStore::new(dir.path()), config, Vec::new());
        let outcome = engine.prepare().await;
        *TEST_HOST.lock().unwrap() = None;
        assert!(outcome.is_err());

        let (name, _, properties, _) = asked
            .started
            .recv_timeout(Duration::from_secs(10))
            .expect("the spawn asked");
        let [pid] = Vec::<u32>::try_from(property(&properties, "PIDs"))
            .unwrap()
            .try_into()
            .unwrap();
        assert_ne!(pid, std::process::id());
        assert_eq!(name, scope_name(pid));
    }
}
