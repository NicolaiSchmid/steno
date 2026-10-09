//! Linux only: the child in a systemd scope of its own (P6 of
//! `.plans/2026-10-07-stable-promotion.md`).
//!
//! A desktop launched by a systemd user session runs in a unit of the
//! user manager, a scope or a service under `app.slice`, and its child
//! joins that unit's cgroup. `systemd-oomd`, which some distributions turn
//! on for `app.slice` (`ManagedOOMMemoryPressure=kill`), kills a whole
//! cgroup: under memory pressure from the child working through a long
//! meeting, it would kill the app with it, and the recording in progress.
//! So right after the spawn the client asks the user manager to start a
//! transient scope beside the app's unit, in the same slice, holding only
//! the child. `systemd-oomd` then weighs the two cgroups apart and picks
//! the child's, by far the larger; the client sees a dead child as after
//! any crash, and the next call starts another, in a scope of its own.
//!
//! The scope is `PartOf` the app's unit, so stopping that unit stops the
//! child too; the child also ends at the app's exit, as everywhere (the
//! `steno-speech-sidecar` crate docs). An app outside a user unit (a
//! system service, an ssh login, a container without a user manager, a
//! distribution without systemd) keeps its child in its own cgroup, as
//! does any failure here, which is logged at info: the app records either
//! way.
//!
//! The request goes over the user bus's Unix socket in
//! `$XDG_RUNTIME_DIR`, where the user manager listens, and carries the
//! child's pid and unit names, nothing else. The child itself still opens
//! nothing.
//!
//! Swift: none; the Mac app runs `FluidAudio` in-process only.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::zvariant::Value;

/// How long the spawn waits for the user manager's answer.
const PATIENCE: Duration = Duration::from_secs(2);

/// Where the app runs: its unit and the slice that holds it, as named in
/// its cgroup's path.
#[derive(Debug, PartialEq, Eq)]
struct Placement<'a> {
    unit: &'a str,
    slice: &'a str,
}

/// The app's unit and slice from `/proc/self/cgroup`, when the app runs
/// in a scope or service of a systemd user manager (a path through
/// `user@<uid>.service`) directly under a slice; `None` otherwise, and on
/// cgroup v1, which has no `0::` line.
fn placement(cgroup: &str) -> Option<Placement<'_>> {
    let path = cgroup.lines().find_map(|line| line.strip_prefix("0::"))?;
    let mut parts = path.rsplit('/');
    let unit = parts.next()?;
    let slice = parts.next()?;
    let in_user_manager =
        parts.any(|part| part.starts_with("user@") && kind(part) == Some("service"));
    (in_user_manager
        && kind(slice) == Some("slice")
        && matches!(kind(unit), Some("scope" | "service")))
    .then_some(Placement { unit, slice })
}

/// A unit's type, the part of its name after the last dot. A name that
/// starts with `_` is one systemd escaped for the cgroup tree, not the
/// unit's, and has none.
fn kind(name: &str) -> Option<&str> {
    let (stem, kind) = name.rsplit_once('.')?;
    (!stem.is_empty() && !name.starts_with('_')).then_some(kind)
}

/// The child's scope, named as the XDG convention for applications'
/// units has it (`app-<id>-<random>.scope`), the pid as the random part.
fn scope_name(pid: u32) -> String {
    format!("app-steno\\x2dspeech\\x2dsidecar-{pid}.scope")
}

/// What the scope is started with.
fn properties<'a>(pid: u32, placement: &Placement<'a>) -> Vec<(&'static str, Value<'a>)> {
    vec![
        ("Description", Value::from("Steno speech sidecar")),
        ("PIDs", Value::from(vec![pid])),
        ("Slice", Value::from(placement.slice)),
        ("PartOf", Value::from(vec![placement.unit])),
        // A scope `systemd-oomd` killed ends failed; it is collected
        // then, not kept for `systemctl --user --failed`.
        ("CollectMode", Value::from("inactive-or-failed")),
    ]
}

/// Asks the user manager on `connection` to start the child's scope;
/// the scope's name once it accepted.
fn start_scope(connection: &Connection, pid: u32, placement: &Placement) -> zbus::Result<String> {
    let name = scope_name(pid);
    let auxiliary: Vec<(&str, Vec<(&str, Value)>)> = Vec::new();
    connection.call_method(
        Some("org.freedesktop.systemd1"),
        "/org/freedesktop/systemd1",
        Some("org.freedesktop.systemd1.Manager"),
        "StartTransientUnit",
        &(name.as_str(), "fail", properties(pid, placement), auxiliary),
    )?;
    Ok(name)
}

/// Moves the child `pid` into a scope of its own when `cgroup` (the
/// app's `/proc/self/cgroup`) places the app in a user unit, over the
/// user bus in `runtime_dir`. The scope's name, or `None` when the app is
/// not in a user unit and nothing was asked. A bus that has not taken the
/// connection by `deadline` is not asked: the spawn stopped waiting then,
/// and a child that died since may be reaped and its pid reused.
fn move_with(
    pid: u32,
    cgroup: &str,
    runtime_dir: &Path,
    deadline: Instant,
) -> zbus::Result<Option<String>> {
    let Some(placement) = placement(cgroup) else {
        return Ok(None);
    };
    let stream = UnixStream::connect(runtime_dir.join("bus"))?;
    let connection = Builder::async_io_unix_stream(stream)
        .method_timeout(PATIENCE)
        .build()?;
    if Instant::now() >= deadline {
        return Err(zbus::Error::Failure(
            "the user bus took the connection too late".to_owned(),
        ));
    }
    start_scope(&connection, pid, &placement).map(Some)
}

/// [`move_with`] on a thread of its own, waited for `patience` at most:
/// the method timeout does not cover the connection's set-up, so a bus
/// that accepts and never answers would otherwise hold the spawn. Such a
/// thread is left to the bus.
fn move_within(
    pid: u32,
    cgroup: String,
    runtime_dir: PathBuf,
    patience: Duration,
) -> zbus::Result<Option<String>> {
    if placement(&cgroup).is_none() {
        return Ok(None);
    }
    let deadline = Instant::now() + patience;
    let (done, outcome) = mpsc::channel();
    std::thread::Builder::new()
        .name(format!("sidecar-{pid}-scope"))
        .spawn(move || {
            let _ = done.send(move_with(pid, &cgroup, &runtime_dir, deadline));
        })?;
    outcome.recv_timeout(patience).unwrap_or_else(|_| {
        Err(zbus::Error::Failure(
            "the user bus did not answer in time".to_owned(),
        ))
    })
}

/// Moves the just-spawned child `pid` into a scope of its own when the
/// app runs in a systemd user unit (the module docs), waiting [`PATIENCE`]
/// at most. Called before the parent waits on the child, so `pid` cannot
/// name another process yet. Never fails: the child stays in the app's
/// cgroup otherwise.
pub(super) fn move_to_own_scope(pid: u32) {
    let Ok(cgroup) = std::fs::read_to_string("/proc/self/cgroup") else {
        return;
    };
    let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty())
    else {
        return;
    };
    match move_within(pid, cgroup, PathBuf::from(runtime_dir), PATIENCE) {
        Ok(Some(scope)) => tracing::info!(pid, %scope, "speech sidecar in a scope of its own"),
        Ok(None) => tracing::debug!(
            pid,
            "the app runs in no systemd user unit; the speech sidecar shares its cgroup"
        ),
        Err(error) => {
            tracing::info!(pid, %error, "the speech sidecar shares the app's cgroup: its scope could not be started");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::BufRead as _;

    use zbus::zvariant::{OwnedObjectPath, OwnedValue};

    use super::*;

    const UWSM: &str = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-graphical.slice/app-Hyprland-steno\\x2ddesktop-1234.scope\n";

    #[test]
    fn the_unit_and_slice_come_from_a_user_managers_cgroup_only() {
        assert_eq!(
            placement(UWSM),
            Some(Placement {
                unit: "app-Hyprland-steno\\x2ddesktop-1234.scope",
                slice: "app-graphical.slice",
            })
        );
        assert_eq!(
            placement(
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-steno\\x2ddesktop@autostart.service"
            ),
            Some(Placement {
                unit: "app-steno\\x2ddesktop@autostart.service",
                slice: "app.slice",
            })
        );
        // A hybrid system's v1 lines are skipped for the unified one.
        assert!(placement(&format!("12:pids:/user.slice\n1:name=systemd:/x\n{UWSM}")).is_some());
        for outside in [
            // A system service, an ssh login, the user manager itself.
            "0::/system.slice/t3code.service",
            "0::/user.slice/user-1000.slice/session-3.scope",
            "0::/user.slice/user-1000.slice/user@1000.service/init.scope",
            // Units below a system service that delegates its subtree.
            "0::/system.slice/runner.service/app.slice/job.scope",
            // A sub-cgroup inside a delegated unit, an escaped name.
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/vte.scope/tab-1",
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/_cgroup.scope",
            // A container's root, cgroup v1 only, nothing.
            "0::/",
            "1:name=systemd:/user.slice/user-1000.slice/user@1000.service/app.slice/a.scope",
            "",
        ] {
            assert_eq!(placement(outside), None, "{outside}");
        }
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
    }

    #[test]
    fn the_child_gets_a_scope_beside_the_apps_unit_and_part_of_it() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let (started, calls) = mpsc::channel();
        let address = format!("unix:path={}", daemon.dir.path().join("bus").display());
        let _manager = Builder::address(address.as_str())
            .unwrap()
            .name("org.freedesktop.systemd1")
            .unwrap()
            .serve_at("/org/freedesktop/systemd1", FakeManager { started })
            .unwrap()
            .build()
            .unwrap();

        let scope = move_within(4242, UWSM.to_owned(), daemon.dir.path().into(), PATIENCE).unwrap();
        assert_eq!(
            scope.as_deref(),
            Some("app-steno\\x2dspeech\\x2dsidecar-4242.scope")
        );
        let (name, mode, properties, auxiliary) = calls.try_recv().expect("one call");
        assert!(auxiliary.is_empty());
        assert_eq!(name, "app-steno\\x2dspeech\\x2dsidecar-4242.scope");
        assert_eq!(mode, "fail");
        let property = |key: &str| {
            let (_, value) = properties.iter().find(|(name, _)| name == key).unwrap();
            value.try_clone().unwrap()
        };
        assert_eq!(Vec::<u32>::try_from(property("PIDs")).unwrap(), [4242]);
        assert_eq!(
            String::try_from(property("Slice")).unwrap(),
            "app-graphical.slice"
        );
        assert_eq!(
            Vec::<String>::try_from(property("PartOf")).unwrap(),
            ["app-Hyprland-steno\\x2ddesktop-1234.scope"]
        );
        assert_eq!(
            String::try_from(property("CollectMode")).unwrap(),
            "inactive-or-failed"
        );
        assert_eq!(properties.len(), 5);

        // Outside a user unit nothing is asked, not even a connection.
        assert_eq!(
            move_within(
                4242,
                "0::/system.slice/steno.service".to_owned(),
                "/nonexistent".into(),
                PATIENCE
            )
            .unwrap(),
            None
        );
        assert!(calls.try_recv().is_err());
    }

    #[test]
    fn no_bus_a_bus_without_the_user_manager_and_a_silent_bus_are_errors_to_log() {
        let away = || Instant::now() + PATIENCE;
        assert!(move_with(1, UWSM, Path::new("/nonexistent"), away()).is_err());

        // A socket that takes the connection and never answers: the wait
        // ends at its patience.
        let silent = tempfile::tempdir().unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(silent.path().join("bus")).unwrap();
        let asked = Instant::now();
        let error = move_within(
            1,
            UWSM.to_owned(),
            silent.path().into(),
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(
            asked.elapsed() < Duration::from_secs(2),
            "{:?}",
            asked.elapsed()
        );
        assert!(error.to_string().contains("in time"), "{error}");

        let Some(daemon) = Daemon::start() else {
            return;
        };
        let error = move_with(1, UWSM, daemon.dir.path(), away()).unwrap_err();
        assert!(
            matches!(&error, zbus::Error::MethodError(name, ..) if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown"),
            "{error}"
        );
        // A bus that took the connection after the deadline is not asked.
        let error = move_with(1, UWSM, daemon.dir.path(), Instant::now()).unwrap_err();
        assert!(error.to_string().contains("too late"), "{error}");
    }
}
