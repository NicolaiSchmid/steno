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
//! waits until the child's cgroup names that scope. A start the manager
//! has not carried out by then is called off, and the child stays in the
//! app's cgroup; a child that joined its scope just before the call-off
//! stays in it. `systemd-oomd` then weighs the two cgroups apart and takes
//! the child's first; the client sees a dead child as after any crash,
//! and the next call starts another, in a scope of its own. The app's own
//! cgroup is still a candidate: speaker diarization runs in the app's
//! process (`crates/steno-diarize/src/onnx.rs`), and pressure that lasts
//! after the child is gone can take the app too.
//!
//! The scope is `PartOf` the app's unit, so stopping that unit stops the
//! child too; the child also ends at the app's exit, as everywhere (the
//! `steno-speech-sidecar` crate docs). A kernel OOM kill of the child does
//! not count against the app's unit either. An app outside a user unit (a
//! system service, an ssh login, a container without a user manager, a
//! distribution without systemd) keeps its child in its own cgroup, as
//! does any failure here, which is logged at info: the app records either
//! way.
//!
//! The request goes over the user bus's Unix socket in
//! `$XDG_RUNTIME_DIR`, where the user manager listens, and carries the
//! child's pid, the unit names and the scope's fixed settings, nothing
//! else. The child itself still opens nothing.
//!
//! - [`move_to_own_scope`]: the one entry point, called by `client` right
//!   after the spawn, with this process's [`System`].
//! - [`move_within`]: the request on a thread of its own, waited for a
//!   bounded time, one at a time.
//! - [`start_scope_over_bus`]: the request itself: the scope's start, the
//!   wait until the child's cgroup names it, and at the deadline the
//!   start called off.
//! - [`placement`]: the app's unit and slice from its cgroup.
//!
//! Swift: none; the Mac app runs `FluidAudio` in-process only.

use std::ffi::OsString;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::zvariant::{OwnedObjectPath, Value};

/// How long the child has to be in its scope. The spawn waits
/// [`POLL_INTERVAL`] and [`CALL_OFF_TIME`] longer, for the request's last
/// read and the call-off.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(2);

/// How often the child's cgroup is read while the manager's job runs.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How long the spawn waits for the call-off at the deadline.
const CALL_OFF_TIME: Duration = Duration::from_millis(100);

/// Set while a request is under way, so a bus that never answers holds
/// one thread and one socket at most, not one per spawn.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// What the move reads from the system; the tests supply their own.
#[derive(Debug, Clone)]
struct System {
    /// The app's `/proc/self/cgroup`.
    cgroup: String,
    /// `$XDG_RUNTIME_DIR`, where the user bus listens.
    runtime_dir: PathBuf,
    /// The child's cgroup file, by pid.
    child_cgroup: fn(u32) -> std::io::Result<String>,
    /// Whether the child, by pid, has ended and waits to be reaped.
    child_ended: fn(u32) -> bool,
}

impl System {
    /// The system with the app's `cgroup` file and `runtime_dir`, read
    /// from `$XDG_RUNTIME_DIR`: `None` unless that is an absolute path.
    fn new(cgroup: String, runtime_dir: Option<OsString>) -> Option<Self> {
        let runtime_dir = runtime_dir
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())?;
        Some(Self {
            cgroup,
            runtime_dir,
            child_cgroup: proc_cgroup,
            child_ended: proc_ended,
        })
    }

    /// This process's: `None` without a cgroup file or an absolute
    /// `$XDG_RUNTIME_DIR`.
    fn current() -> Option<Self> {
        let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
        Self::new(cgroup, std::env::var_os("XDG_RUNTIME_DIR"))
    }
}

/// Stands in for [`System::current`] while set, for the spawn's test.
#[cfg(test)]
static TEST_SYSTEM: std::sync::Mutex<Option<System>> = std::sync::Mutex::new(None);

/// The system the spawn reads: [`System::current`], or in the tests
/// [`TEST_SYSTEM`] while set.
fn system() -> Option<System> {
    #[cfg(test)]
    if let Some(system) = TEST_SYSTEM
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return Some(system);
    }
    System::current()
}

/// The cgroup file of process `pid`.
fn proc_cgroup(pid: u32) -> std::io::Result<String> {
    std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
}

/// Whether process `pid` has ended and waits to be reaped: its state in
/// `/proc/<pid>/stat`, after the command's closing parenthesis, is `Z` or
/// `X`. A file that cannot be read says nothing.
fn proc_ended(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .is_some_and(|(_, rest)| matches!(rest.trim_start().chars().next(), Some('Z' | 'X')))
    })
}

/// Why the child is not in a scope of its own.
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
    #[error(
        "the user manager had not moved the child into its scope in time; the start was called off"
    )]
    CalledOff,
    #[error("the user manager started the scope without the child; the scope was stopped")]
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

/// Calls the user manager's `method` on `connection`; the job it queued.
fn manager<B>(connection: &Connection, method: &str, body: &B) -> zbus::Result<OwnedObjectPath>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    connection
        .call_method(
            Some("org.freedesktop.systemd1"),
            "/org/freedesktop/systemd1",
            Some("org.freedesktop.systemd1.Manager"),
            method,
            body,
        )?
        .body()
        .deserialize()
}

/// Cancels the manager's `job`; an error once the job has run.
fn cancel(connection: &Connection, job: &OwnedObjectPath) -> zbus::Result<()> {
    connection.call_method(
        Some("org.freedesktop.systemd1"),
        job.as_str(),
        Some("org.freedesktop.systemd1.Job"),
        "Cancel",
        &(),
    )?;
    Ok(())
}

/// Moves the child `pid` into a scope of its own beside the app's unit
/// at `placement`, over `system`'s user bus; the scope's name once the
/// child's cgroup names it. Every call waits at most the time left
/// before `deadline` when the connection started. A bus that has not
/// taken the connection by `deadline` is not asked: the spawn stopped
/// waiting then, and a child that died since may be reaped and its pid
/// reused.
///
/// `StartTransientUnit` answers once the manager queued the job, and the
/// job moves the child later. A child not in its scope by `deadline`, or
/// one that has ended first, has the start called off: the manager handles
/// one call at a time, so a job it cancels never ran, and the child stays
/// in the app's cgroup. A job it cannot cancel has run: a child in its
/// scope then stays there, and a scope without it (the child had ended)
/// is stopped. A start the manager answers only after `deadline` cannot
/// be called off; it may still move the child, which then stays in its
/// scope.
fn start_scope_over_bus(
    pid: u32,
    placement: &Placement,
    system: &System,
    deadline: Instant,
) -> Result<String, ScopeError> {
    let stream = UnixStream::connect(system.runtime_dir.join("bus")).map_err(ScopeError::NoBus)?;
    let connection = Builder::async_io_unix_stream(stream)
        .method_timeout(deadline.saturating_duration_since(Instant::now()))
        .build()?;
    if Instant::now() >= deadline {
        return Err(ScopeError::Late);
    }
    let name = scope_name(pid);
    let auxiliary: Vec<(&str, Vec<(&str, Value)>)> = Vec::new();
    let job = manager(
        &connection,
        "StartTransientUnit",
        &(name.as_str(), "fail", properties(pid, placement), auxiliary),
    )?;
    let in_scope = || (system.child_cgroup)(pid).is_ok_and(|cgroup| holds(&cgroup, &name));
    while !in_scope() {
        if Instant::now() >= deadline || (system.child_ended)(pid) {
            if cancel(&connection, &job).is_ok() {
                return Err(ScopeError::CalledOff);
            }
            if in_scope() {
                return Ok(name);
            }
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

/// [`start_scope_over_bus`] on a thread of its own, with `timeout` until
/// its deadline, when `system` places the app in a user unit; `None` when
/// it does not and nothing was asked. The spawn waits [`POLL_INTERVAL`]
/// and [`CALL_OFF_TIME`] past the deadline, for the request's last read
/// and the call-off. The method timeout does not cover the connection's
/// set-up, so a bus that accepts and never answers would otherwise hold
/// the spawn. A thread still waiting then runs on until the bus answers
/// or closes, and logs its outcome itself; while it does, `in_flight` is
/// set and later spawns do not ask.
fn move_within(
    pid: u32,
    system: System,
    timeout: Duration,
    in_flight: &'static AtomicBool,
) -> Result<Option<String>, ScopeError> {
    let Some(placement) = placement(&system.cgroup) else {
        return Ok(None);
    };
    if in_flight.swap(true, Ordering::SeqCst) {
        return Err(ScopeError::Busy);
    }
    let guard = InFlight(in_flight);
    let deadline = Instant::now() + timeout;
    let (done, outcome) = mpsc::channel();
    std::thread::Builder::new()
        .name(format!("sidecar-{pid}-scope"))
        .spawn(move || {
            let outcome = start_scope_over_bus(pid, &placement, &system, deadline);
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
                        "the speech sidecar stays in the app's cgroup, after the spawn stopped waiting"
                    ),
                }
            }
        })
        .map_err(ScopeError::Thread)?;
    outcome
        .recv_timeout(timeout + POLL_INTERVAL + CALL_OFF_TIME)
        .unwrap_or(Err(ScopeError::Silent))
        .map(Some)
}

/// Moves the just-spawned child `pid` into a scope of its own when the
/// app runs in a systemd user unit (the module docs), waiting about
/// [`ANSWER_TIMEOUT`] at most. Called before the parent waits on the
/// child, so `pid` cannot name another process yet. Never fails: the child
/// stays in the app's cgroup otherwise.
pub(super) fn move_to_own_scope(pid: u32) {
    let Some(system) = system() else {
        return;
    };
    match move_within(pid, system, ANSWER_TIMEOUT, &IN_FLIGHT) {
        Ok(Some(scope)) => tracing::info!(pid, %scope, "speech sidecar in a scope of its own"),
        Ok(None) => tracing::debug!(
            pid,
            "the app runs in no systemd user unit; the speech sidecar stays in the app's cgroup"
        ),
        Err(ScopeError::Silent) => tracing::info!(
            pid,
            "the user bus did not answer in time; the speech sidecar stays in the app's cgroup unless the user manager still moves it"
        ),
        Err(error) => {
            tracing::info!(pid, %error, "the speech sidecar stays in the app's cgroup");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::BufRead as _;
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    use std::sync::atomic::AtomicUsize;

    use steno_core::SpeechEngine as _;
    use zbus::zvariant::OwnedValue;

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

    /// A child whose cgroup file cannot be read.
    const UNREADABLE: fn(u32) -> std::io::Result<String> =
        |_| Err(std::io::Error::other("unreadable"));

    /// The app in a uwsm-style scope, its user bus in `runtime_dir`, its
    /// child running with the cgroup `child_cgroup` reads.
    fn uwsm_system(runtime_dir: &Path, child_cgroup: fn(u32) -> std::io::Result<String>) -> System {
        System {
            cgroup: UWSM.to_owned(),
            runtime_dir: runtime_dir.to_owned(),
            child_cgroup,
            child_ended: |_| false,
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
        assert!(POLL_INTERVAL + CALL_OFF_TIME <= Duration::from_millis(110));
        let own = std::fs::read_to_string("/proc/self/cgroup").unwrap();
        assert_eq!(proc_cgroup(std::process::id()).unwrap(), own);
        assert!(proc_cgroup(u32::MAX).is_err());
        let system = System::new(own.clone(), Some("/run/user/1000".into())).unwrap();
        assert_eq!(system.cgroup, own);
        assert_eq!(system.runtime_dir, Path::new("/run/user/1000"));
        assert_eq!((system.child_cgroup)(std::process::id()).unwrap(), own);
        assert!((system.child_cgroup)(u32::MAX).is_err());
        assert!(!(system.child_ended)(std::process::id()));
        assert!(System::new(own.clone(), Some("run/user/1000".into())).is_none());
        assert!(System::new(own.clone(), None).is_none());
        if let Some(current) = System::current() {
            assert_eq!(current.cgroup, own);
            assert!(current.runtime_dir.is_absolute());
        }
        assert!(holds(&IN_ITS_SCOPE(7).unwrap(), &scope_name(7)));
        assert!(!holds(UWSM, &scope_name(7)));
    }

    #[test]
    fn a_child_that_has_ended_is_seen_before_it_is_reaped() {
        // `sh` waits on its stdin, then ends once the pipe closes.
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "read line"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!proc_ended(child.id()));
        drop(child.stdin.take());
        let started = Instant::now();
        while !proc_ended(child.id()) {
            assert!(started.elapsed() < Duration::from_secs(10), "never ended");
            std::thread::sleep(POLL_INTERVAL);
        }
        // A zombie stays one until it is reaped.
        std::thread::sleep(Duration::from_millis(100));
        assert!(proc_ended(child.id()));
        child.wait().unwrap();
        assert!(!proc_ended(std::process::id()));
        assert!(!proc_ended(u32::MAX));
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

    /// The path of the fake's one job.
    const JOB: &str = "/org/freedesktop/systemd1/job/1";

    /// How the fake's job answers `Cancel`.
    #[derive(Clone, Copy)]
    enum Job {
        /// Still queued: it is cancelled.
        Queued,
        /// Run already: the call errs, as for any job that is gone, after
        /// the job's move set the flag given.
        Ran(Option<&'static AtomicBool>),
    }

    /// The user manager as far as a transient unit goes.
    struct FakeManager {
        started: mpsc::Sender<Started>,
        stopped: mpsc::Sender<(String, String)>,
        /// How long `StartTransientUnit` takes to answer.
        delay: Duration,
    }

    /// The job `StartTransientUnit` queued.
    struct FakeJob {
        job: Job,
        cancelled: mpsc::Sender<()>,
    }

    /// What the fake manager was asked, in order, per method.
    struct Asked {
        started: mpsc::Receiver<Started>,
        stopped: mpsc::Receiver<(String, String)>,
        cancelled: mpsc::Receiver<()>,
        _connection: Connection,
    }

    impl FakeManager {
        /// The fake on `daemon`'s bus under the manager's name, its job
        /// answering as `job` says.
        fn serve(daemon: &Daemon, job: Job) -> Asked {
            Self::serve_slow(daemon, job, Duration::ZERO)
        }

        /// [`FakeManager::serve`], answering `StartTransientUnit` after
        /// `delay`.
        fn serve_slow(daemon: &Daemon, job: Job, delay: Duration) -> Asked {
            let (started, started_calls) = mpsc::channel();
            let (stopped, stopped_calls) = mpsc::channel();
            let (cancelled, cancel_calls) = mpsc::channel();
            let address = format!("unix:path={}", daemon.path().join("bus").display());
            let manager = Self {
                started,
                stopped,
                delay,
            };
            let connection = Builder::address(address.as_str())
                .unwrap()
                .name("org.freedesktop.systemd1")
                .unwrap()
                .serve_at("/org/freedesktop/systemd1", manager)
                .unwrap()
                .serve_at(JOB, FakeJob { job, cancelled })
                .unwrap()
                .build()
                .unwrap();
            Asked {
                started: started_calls,
                stopped: stopped_calls,
                cancelled: cancel_calls,
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
            std::thread::sleep(self.delay);
            OwnedObjectPath::try_from(JOB).unwrap()
        }

        fn stop_unit(&self, name: String, mode: String) -> OwnedObjectPath {
            let _ = self.stopped.send((name, mode));
            OwnedObjectPath::try_from("/org/freedesktop/systemd1/job/2").unwrap()
        }
    }

    #[zbus::interface(name = "org.freedesktop.systemd1.Job")]
    impl FakeJob {
        fn cancel(&self) -> zbus::fdo::Result<()> {
            let _ = self.cancelled.send(());
            match self.job {
                Job::Queued => Ok(()),
                Job::Ran(joined) => {
                    if let Some(joined) = joined {
                        joined.store(true, Ordering::SeqCst);
                    }
                    Err(zbus::fdo::Error::UnknownObject(format!(
                        "Unknown object '{JOB}'."
                    )))
                }
            }
        }
    }

    /// The value of property `key` in a `StartTransientUnit` call.
    fn property(properties: &[(String, OwnedValue)], key: &str) -> OwnedValue {
        let (_, value) = properties.iter().find(|(name, _)| name == key).unwrap();
        value.try_clone().unwrap()
    }

    /// Waits until the request's thread cleared `in_flight`.
    fn wait_until_ended(in_flight: &AtomicBool) {
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
        let asked = FakeManager::serve(&daemon, Job::Queued);

        let scope = move_within(
            4242,
            uwsm_system(daemon.path(), IN_ITS_SCOPE),
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
        assert!(asked.cancelled.try_recv().is_err());
        assert!(asked.stopped.try_recv().is_err());
        assert!(!IN_FLIGHT.load(Ordering::SeqCst));
    }

    #[test]
    fn a_child_seen_in_its_scope_after_a_few_reads_is_moved() {
        static READS: AtomicUsize = AtomicUsize::new(0);
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = FakeManager::serve(&daemon, Job::Queued);
        let after_three_reads: fn(u32) -> std::io::Result<String> = |pid| {
            if READS.fetch_add(1, Ordering::SeqCst) < 3 {
                LEFT_BEHIND(pid)
            } else {
                IN_ITS_SCOPE(pid)
            }
        };

        let scope = start_scope_over_bus(
            4242,
            &placement(UWSM).unwrap(),
            &uwsm_system(daemon.path(), after_three_reads),
            Instant::now() + ANSWER_TIMEOUT,
        )
        .unwrap();
        assert_eq!(scope, scope_name(4242));
        assert_eq!(READS.load(Ordering::SeqCst), 4);
        assert!(asked.cancelled.try_recv().is_err());
        assert!(asked.stopped.try_recv().is_err());
    }

    #[test]
    fn a_child_not_seen_in_its_scope_in_time_has_the_start_called_off() {
        static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = FakeManager::serve(&daemon, Job::Queued);

        // The spawn waits for the call-off, so its outcome is the
        // thread's.
        for child_cgroup in [LEFT_BEHIND, UNREADABLE] {
            let error = move_within(
                4242,
                uwsm_system(daemon.path(), child_cgroup),
                Duration::from_millis(300),
                &IN_FLIGHT,
            )
            .unwrap_err();
            assert!(matches!(error, ScopeError::CalledOff), "{error}");
            assert!(!IN_FLIGHT.load(Ordering::SeqCst));
            assert!(asked.started.try_recv().is_ok());
            assert!(asked.cancelled.try_recv().is_ok());
        }
        assert!(asked.stopped.try_recv().is_err());
    }

    #[test]
    fn a_child_that_has_ended_has_the_start_called_off_at_once() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = FakeManager::serve(&daemon, Job::Queued);
        let ended = System {
            child_ended: |_| true,
            ..uwsm_system(daemon.path(), LEFT_BEHIND)
        };

        let started = Instant::now();
        let error = start_scope_over_bus(
            4242,
            &placement(UWSM).unwrap(),
            &ended,
            started + ANSWER_TIMEOUT,
        )
        .unwrap_err();
        assert!(matches!(error, ScopeError::CalledOff), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert!(asked.cancelled.try_recv().is_ok());
        assert!(asked.stopped.try_recv().is_err());
    }

    #[test]
    fn a_child_that_joins_as_the_start_is_called_off_stays_in_its_scope() {
        static JOINED: AtomicBool = AtomicBool::new(false);
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = FakeManager::serve(&daemon, Job::Ran(Some(&JOINED)));
        let joins_late: fn(u32) -> std::io::Result<String> = |pid| {
            if JOINED.load(Ordering::SeqCst) {
                IN_ITS_SCOPE(pid)
            } else {
                LEFT_BEHIND(pid)
            }
        };

        let scope = start_scope_over_bus(
            4242,
            &placement(UWSM).unwrap(),
            &uwsm_system(daemon.path(), joins_late),
            Instant::now() + Duration::from_millis(300),
        )
        .unwrap();
        assert_eq!(scope, scope_name(4242));
        assert!(asked.cancelled.try_recv().is_ok());
        assert!(asked.stopped.try_recv().is_err());
    }

    #[test]
    fn a_scope_that_started_without_the_child_is_stopped() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = FakeManager::serve(&daemon, Job::Ran(None));

        let error = start_scope_over_bus(
            4242,
            &placement(UWSM).unwrap(),
            &uwsm_system(daemon.path(), LEFT_BEHIND),
            Instant::now() + Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(matches!(error, ScopeError::NotMoved), "{error}");
        assert!(asked.cancelled.try_recv().is_ok());
        assert_eq!(
            asked.stopped.try_recv().expect("the scope stopped"),
            (scope_name(4242), "replace".to_owned())
        );
    }

    #[test]
    fn a_manager_that_answers_after_the_deadline_holds_the_request_no_longer() {
        static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = FakeManager::serve_slow(&daemon, Job::Queued, Duration::from_millis(1500));

        let timeout = Duration::from_millis(300);
        let started = Instant::now();
        let outcome = move_within(
            4242,
            uwsm_system(daemon.path(), IN_ITS_SCOPE),
            timeout,
            &IN_FLIGHT,
        );
        assert!(outcome.is_err(), "{outcome:?}");
        wait_until_ended(&IN_FLIGHT);
        assert!(
            started.elapsed() < timeout + Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
        assert!(asked.started.try_recv().is_ok());
        assert!(asked.cancelled.try_recv().is_err());
    }

    #[test]
    fn outside_a_user_unit_nothing_is_asked() {
        static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
        let outside = System {
            cgroup: "0::/system.slice/steno.service".to_owned(),
            ..uwsm_system(Path::new("/nonexistent"), IN_ITS_SCOPE)
        };
        assert!(matches!(
            move_within(4242, outside, ANSWER_TIMEOUT, &IN_FLIGHT),
            Ok(None)
        ));
        assert!(!IN_FLIGHT.load(Ordering::SeqCst));
    }

    #[test]
    fn a_later_spawn_does_not_ask_while_one_waits() {
        static IN_FLIGHT: AtomicBool = AtomicBool::new(true);
        assert!(matches!(
            move_within(
                1,
                uwsm_system(Path::new("/nonexistent"), IN_ITS_SCOPE),
                ANSWER_TIMEOUT,
                &IN_FLIGHT
            ),
            Err(ScopeError::Busy)
        ));
        assert!(IN_FLIGHT.load(Ordering::SeqCst));
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
    fn a_missing_bus_or_manager_is_an_error() {
        let uwsm = placement(UWSM).unwrap();
        let in_time = || Instant::now() + ANSWER_TIMEOUT;
        assert!(matches!(
            start_scope_over_bus(
                1,
                &uwsm,
                &uwsm_system(Path::new("/nonexistent"), IN_ITS_SCOPE),
                in_time()
            ),
            Err(ScopeError::NoBus(_))
        ));
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let error = start_scope_over_bus(
            1,
            &uwsm,
            &uwsm_system(daemon.path(), IN_ITS_SCOPE),
            in_time(),
        )
        .unwrap_err();
        assert!(
            matches!(&error, ScopeError::Bus(zbus::Error::MethodError(name, ..)) if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown"),
            "{error}"
        );
    }

    /// A socket that takes the connection and never answers: the wait
    /// ends at its timeout, and once the socket closes the thread ends.
    #[test]
    fn a_bus_that_never_answers_ends_the_wait_at_its_timeout() {
        static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
        let silent = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(silent.path().join("bus")).unwrap();
        let asked = Instant::now();
        let error = move_within(
            1,
            uwsm_system(silent.path(), IN_ITS_SCOPE),
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
        wait_until_ended(&IN_FLIGHT);
    }

    /// A bus that takes the connection after the deadline is not asked,
    /// whether the deadline passed before the connection or during its
    /// set-up.
    #[test]
    fn a_bus_that_takes_the_connection_too_late_is_not_asked() {
        static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = FakeManager::serve(&daemon, Job::Queued);
        assert!(matches!(
            start_scope_over_bus(
                1,
                &placement(UWSM).unwrap(),
                &uwsm_system(daemon.path(), IN_ITS_SCOPE),
                Instant::now()
            ),
            Err(ScopeError::Late)
        ));
        let slow = slow_bus(daemon.path(), Duration::from_millis(500));
        let error = move_within(
            1,
            uwsm_system(slow.path(), IN_ITS_SCOPE),
            Duration::from_millis(200),
            &IN_FLIGHT,
        )
        .unwrap_err();
        assert!(matches!(error, ScopeError::Silent), "{error}");
        wait_until_ended(&IN_FLIGHT);
        assert!(asked.started.try_recv().is_err());
    }

    /// The spawn itself asks for the child's scope: `sh` stands in for
    /// the sidecar, exits at once and never says it is ready.
    #[tokio::test]
    async fn the_spawn_moves_its_child() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = FakeManager::serve(&daemon, Job::Queued);
        *TEST_SYSTEM.lock().unwrap() = Some(uwsm_system(daemon.path(), IN_ITS_SCOPE));
        let dir = tempfile::tempdir().unwrap();
        let mut config = SidecarConfig::new("/bin/sh");
        config.args = vec!["-c".into(), "exit 0".into()];
        let engine =
            SidecarSpeechEngine::with_assets(ModelStore::new(dir.path()), config, Vec::new());
        let outcome = engine.prepare().await;
        *TEST_SYSTEM.lock().unwrap() = None;
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
