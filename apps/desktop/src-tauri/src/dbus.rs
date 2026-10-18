//! Linux: what the shell's D-Bus clients share. Each runs on a thread of
//! its own (`spawn_client`) over a connection whose calls wait five
//! seconds at most (`patient`), so a frozen bus holds only that thread.
//! `tests::Daemon` is the private bus their tests run on.

use std::time::Duration;

use zbus::blocking::connection::Builder;
use zbus::blocking::{Connection, Proxy};
use zbus::names::{BusName, OwnedUniqueName};
use zbus::proxy::CacheProperties;

/// How long a client waits for a method's answer, so a frozen bus cannot
/// make `EndSession` miss its deadline.
const CALL_PATIENCE: Duration = Duration::from_secs(5);

// Longer would let a frozen bus hold `EndSession` past the session
// manager's patience.
const _: () = assert!(CALL_PATIENCE.as_millis() <= 5_000);

/// The connection `builder` makes, with `CALL_PATIENCE` on every call.
pub fn patient(builder: Builder<'_>) -> zbus::Result<Connection> {
    builder.method_timeout(CALL_PATIENCE).build()
}

/// Runs `client` on a thread named `name`; an error is logged with `gap`,
/// what the app is left without.
pub fn spawn_client(
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

/// A proxy that caches no property, so each read asks the bus.
pub fn proxy<'a>(
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

pub fn bus(connection: &Connection) -> zbus::Result<zbus::blocking::fdo::DBusProxy<'_>> {
    zbus::blocking::fdo::DBusProxy::builder(connection)
        .cache_properties(CacheProperties::No)
        .build()
}

/// The unique name that owns `name`, none when no peer does. Asking starts
/// no service.
pub fn owner_of(connection: &Connection, name: &str) -> zbus::Result<Option<OwnedUniqueName>> {
    match bus(connection)?.get_name_owner(BusName::try_from(name)?) {
        Ok(owner) => Ok(Some(owner)),
        Err(zbus::fdo::Error::NameHasNoOwner(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::BufRead as _;

    use super::*;

    /// A private `dbus-daemon`, ended with the value.
    pub(crate) struct Daemon {
        child: std::process::Child,
        address: String,
    }

    impl Daemon {
        pub(crate) fn start() -> Option<Self> {
            Self::start_at(&format!("unix:tmpdir={}", std::env::temp_dir().display()))
        }

        /// The daemon on `address`; `None` when `dbus-daemon` is missing,
        /// unless `STENO_REQUIRE_DBUS_TEST` makes that a failure.
        pub(crate) fn start_at(address: &str) -> Option<Self> {
            let spawned = std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
                .arg(format!("--address={address}"))
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

        pub(crate) fn builder(&self) -> Builder<'_> {
            Builder::address(self.address.as_str()).unwrap()
        }

        pub(crate) fn connect(&self) -> Connection {
            patient(self.builder()).unwrap()
        }

        pub(crate) fn end(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            self.end();
        }
    }

    /// A call to a silent peer gives up after `CALL_PATIENCE`.
    #[test]
    fn a_patient_call_gives_up() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let silent = daemon.builder().build().unwrap();
        let peer = silent.unique_name().unwrap().to_string();
        let caller = daemon.connect();
        let (done, answered) = std::sync::mpsc::channel();
        let started = std::time::Instant::now();
        std::thread::spawn(move || {
            let answer = proxy(&caller, &peer, "/", "org.steno.Silent")
                .and_then(|proxy| proxy.call_method("Hush", &()));
            let _ = done.send((answer.map(drop), started.elapsed()));
        });
        let margin = Duration::from_secs(3);
        let (answer, waited) = answered
            .recv_timeout(CALL_PATIENCE + margin)
            .expect("the call gave up in time");
        assert!(answer.is_err(), "{answer:?}");
        assert!(waited >= CALL_PATIENCE, "{waited:?}");
        drop(silent);
    }
}
