//! Linux: whether something on the desktop shows the tray's icon. The icon
//! is a status notifier item, which a host shows: KDE Plasma's panel,
//! Waybar, Omarchy's bar, GNOME with the `AppIndicator` extension. Hosts
//! register with a watcher, which owns `org.kde.StatusNotifierWatcher` on
//! the session bus and says through its `IsStatusNotifierHostRegistered`
//! property whether one did. Stock GNOME runs neither, and a watcher can
//! run with no host (KDE's `kded`, started for a KDE app under another
//! desktop), so the shell asks for both.
//!
//! A thread follows them for the whole run (`follow`): it reads the
//! watcher's owner and the property at launch, and again whenever the
//! name changes owner (`NameOwnerChanged`) or the watcher says a host
//! registered or went (`StatusNotifierHostRegistered`,
//! `StatusNotifierHostUnregistered`). The answer is kept in `HOSTED`, which
//! a close of the main window reads without asking the bus (`has_host` in
//! `tray.rs`): with a host the window hides behind the tray, without one
//! the close quits through Quit's path, which saves a recording in
//! progress first (`main.rs`). Until the first reading, with no session
//! bus, and once the bus has closed, the shell counts no host: closing
//! the main window then ends the app instead of leaving it running
//! unseen, the safe side. An `XEmbed`-only tray is not asked for and
//! counts as none too.
//!
//! The property is asked of the watcher's unique name, so asking never
//! starts a watcher the bus could activate. A watcher that does not answer
//! it still counts as a host, as its name alone did before the property
//! was asked.
//!
//! Swift: none needed; an `NSStatusItem` always shows in the menu bar.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc;

use zbus::blocking::connection::Builder;
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type;

use crate::session_end::{owner_of, patient, proxy, spawn_client};

/// The name a status notifier watcher owns on the session bus, and the
/// interface it serves.
pub const WATCHER: &str = "org.kde.StatusNotifierWatcher";

/// The watcher's object.
const WATCHER_PATH: &str = "/StatusNotifierWatcher";

/// The watcher's property that says whether a host registered.
const HOST_REGISTERED: &str = "IsStatusNotifierHostRegistered";

/// The watcher's signals that change `HOST_REGISTERED`; KDE's watcher
/// sends both, the specification names only the first.
const HOST_SIGNALS: [&str; 2] = [
    "StatusNotifierHostRegistered",
    "StatusNotifierHostUnregistered",
];

/// What the session bus says about the watcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watcher {
    /// No peer owns `WATCHER`.
    Absent,
    /// A peer owns it; `host` is its `HOST_REGISTERED`, none when it did
    /// not answer.
    Present { host: Option<bool> },
}

/// Whether a host shows the icon, from a reading of the bus: a watcher
/// that says a host registered, or one that does not say; no watcher, a
/// watcher with no host, or a bus that failed the reading is none.
pub fn shows_icon(reading: &zbus::Result<Watcher>) -> bool {
    match reading {
        Ok(Watcher::Present {
            host: Some(registered),
        }) => *registered,
        Ok(Watcher::Present { host: None }) => true,
        Ok(Watcher::Absent) | Err(_) => false,
    }
}

/// One reading: the watcher's owner (`GetNameOwner`), then its
/// `HOST_REGISTERED`, asked of that unique name.
fn read(session: &Connection) -> zbus::Result<Watcher> {
    let Some(owner) = owner_of(session, WATCHER)? else {
        return Ok(Watcher::Absent);
    };
    let host = proxy(session, owner.as_str(), WATCHER_PATH, WATCHER)?
        .get_property::<bool>(HOST_REGISTERED)
        .inspect_err(|error| tracing::debug!(%error, "the tray's watcher has no host property"))
        .ok();
    Ok(Watcher::Present { host })
}

/// What the follower last found; no host until its first reading.
#[derive(Debug, Default)]
pub struct Hosted(AtomicU8);

/// `Hosted`'s values.
const UNREAD: u8 = 0;
const NO_HOST: u8 = 1;
const HOST: u8 = 2;

impl Hosted {
    pub const fn new() -> Self {
        Self(AtomicU8::new(UNREAD))
    }

    /// Whether a host shows the icon, as last read.
    pub fn shown(&self) -> bool {
        self.0.load(Ordering::SeqCst) == HOST
    }

    /// Keeps a reading; true when it changes what the shell counts, or is
    /// the first.
    fn note(&self, shown: bool) -> bool {
        let value = if shown { HOST } else { NO_HOST };
        self.0.swap(value, Ordering::SeqCst) != value
    }
}

/// The shell's reading, which `tray::has_host` reads.
pub static HOSTED: Hosted = Hosted::new();

/// Starts the follower on the session bus, when there is one; without
/// one the shell counts no host.
pub fn follow() {
    if !crate::session_bus_named() {
        tracing::warn!("no session bus, so no tray host; closing the main window quits Steno");
        return;
    }
    spawn_client(
        "steno-tray-host",
        "no tray host is followed, so closing the main window quits Steno",
        || follow_on(&patient(Builder::session()?)?, &HOSTED),
    );
}

/// Keeps `hosted` up to date from `session` until the bus closes: a
/// reading now, and one after every change of the watcher's owner or of
/// its hosts. Two threads forward the signals, so neither stream waits
/// while a reading runs; the readings run here, one at a time, and a burst
/// of signals makes one reading.
fn follow_on(session: &Connection, hosted: &Hosted) -> zbus::Result<()> {
    let owners = zbus::MatchRule::builder()
        .msg_type(Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .add_arg(WATCHER)?
        .build();
    // Any sender: the watcher's unique name changes with its owner.
    let hosts = zbus::MatchRule::builder()
        .msg_type(Type::Signal)
        .interface(WATCHER)?
        .path(WATCHER_PATH)?
        .build();
    // Subscribed before the first reading, so no change falls between.
    let owners = MessageIterator::for_match_rule(owners, session, None)?;
    let hosts = MessageIterator::for_match_rule(hosts, session, None)?;
    let (nudge, nudged) = mpsc::channel();
    std::thread::scope(|scope| -> zbus::Result<()> {
        let forward = |signals: MessageIterator, wanted: fn(&zbus::Message) -> bool| {
            let nudge = nudge.clone();
            move || {
                for signal in signals {
                    let Ok(signal) = signal else {
                        return;
                    };
                    if wanted(&signal) && nudge.send(()).is_err() {
                        return;
                    }
                }
            }
        };
        std::thread::Builder::new()
            .name("steno-tray-owner".to_owned())
            .spawn_scoped(scope, forward(owners, |_| true))?;
        std::thread::Builder::new()
            .name("steno-tray-hosts".to_owned())
            .spawn_scoped(scope, forward(hosts, is_host_signal))?;
        drop(nudge);
        loop {
            let reading = read(session);
            let shown = shows_icon(&reading);
            if hosted.note(shown) {
                if shown {
                    tracing::info!("a tray host shows the tray icon");
                } else {
                    tracing::warn!(
                        reading = ?reading,
                        "no tray host shows the tray icon; closing the main window quits Steno"
                    );
                }
            }
            if nudged.recv().is_err() {
                break;
            }
            while nudged.try_recv().is_ok() {}
        }
        if hosted.note(false) {
            tracing::warn!("the session bus closed; closing the main window quits Steno");
        }
        Ok(())
    })?;
    Err(zbus::Error::Failure("the session bus closed".to_owned()))
}

/// Whether `signal` is one of the watcher's `HOST_SIGNALS`.
fn is_host_signal(signal: &zbus::Message) -> bool {
    signal
        .header()
        .member()
        .is_some_and(|member| HOST_SIGNALS.contains(&member.as_str()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::session_end::tests::Daemon;

    /// A watcher with a host, a watcher with none, one that does not say,
    /// no watcher, and a failed reading.
    #[test]
    fn a_host_shows_the_icon_only_where_the_watcher_says_one_registered() {
        let present = |host| Ok(Watcher::Present { host });
        assert!(shows_icon(&present(Some(true))));
        assert!(!shows_icon(&present(Some(false))));
        assert!(shows_icon(&present(None)));
        assert!(!shows_icon(&Ok(Watcher::Absent)));
        assert!(!shows_icon(&Err(zbus::Error::Failure("no bus".to_owned()))));
    }

    /// No host until the first reading; a reading is news when it is the
    /// first or changes the answer.
    #[test]
    fn the_first_reading_and_each_change_are_news() {
        let hosted = Hosted::new();
        assert!(!hosted.shown());
        assert!(hosted.note(false));
        assert!(!hosted.note(false));
        assert!(hosted.note(true));
        assert!(hosted.shown());
        assert!(!hosted.note(true));
        assert!(hosted.note(false));
        assert!(!hosted.shown());
    }

    /// The watcher as the follower asks it: the property, and the signals
    /// sent by hand (`Connection::emit_signal`).
    struct FakeWatcher {
        registered: bool,
    }

    #[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
    impl FakeWatcher {
        #[zbus(property)]
        fn is_status_notifier_host_registered(&self) -> bool {
            self.registered
        }
    }

    /// A watcher without the host property: only the specification's
    /// `ProtocolVersion`.
    struct BareWatcher {
        version: i32,
    }

    #[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
    impl BareWatcher {
        #[zbus(property)]
        fn protocol_version(&self) -> i32 {
            self.version
        }
    }

    fn serve(daemon: &Daemon, watcher: impl zbus::object_server::Interface) -> Connection {
        daemon
            .builder()
            .name(WATCHER)
            .unwrap()
            .serve_at(WATCHER_PATH, watcher)
            .unwrap()
            .build()
            .unwrap()
    }

    /// The follower on `daemon`, its reading in the returned `Hosted`.
    fn follow_daemon(daemon: &Daemon) -> Arc<Hosted> {
        let hosted = Arc::new(Hosted::new());
        let (session, kept) = (daemon.connect(), hosted.clone());
        std::thread::spawn(move || follow_on(&session, &kept));
        hosted
    }

    /// Waits until `hosted` reads `shown` after its first reading, ten
    /// seconds at most.
    fn reads(hosted: &Hosted, shown: bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let wanted = if shown { HOST } else { NO_HOST };
        while hosted.0.load(Ordering::SeqCst) != wanted && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(hosted.shown(), shown);
        assert_ne!(hosted.0.load(Ordering::SeqCst), UNREAD);
    }

    /// No watcher is no host; a watcher with a host that comes later is
    /// one; it going is none again.
    #[test]
    fn the_follower_sees_a_watcher_come_and_go() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let hosted = follow_daemon(&daemon);
        reads(&hosted, false);
        let watcher = serve(&daemon, FakeWatcher { registered: true });
        reads(&hosted, true);
        drop(watcher);
        reads(&hosted, false);
    }

    /// A watcher with no host is none until a host registers, and none
    /// again once the host goes.
    #[test]
    fn a_watcher_without_a_host_shows_nothing_until_one_registers() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let watcher = serve(&daemon, FakeWatcher { registered: false });
        let hosted = follow_daemon(&daemon);
        reads(&hosted, false);
        let host = |registered: bool, signal: &str| {
            let interface = watcher
                .object_server()
                .interface::<_, FakeWatcher>(WATCHER_PATH)
                .unwrap();
            interface.get_mut().registered = registered;
            watcher
                .emit_signal(None::<&str>, WATCHER_PATH, WATCHER, signal, &())
                .unwrap();
        };
        host(true, HOST_SIGNALS[0]);
        reads(&hosted, true);
        host(false, HOST_SIGNALS[1]);
        reads(&hosted, false);
    }

    /// A watcher that does not answer the property counts as a host.
    #[test]
    fn a_watcher_without_the_property_counts_as_a_host() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let _watcher = serve(&daemon, BareWatcher { version: 0 });
        let hosted = follow_daemon(&daemon);
        reads(&hosted, true);
    }

    /// The bus closing ends the follower with an error and no host.
    #[test]
    fn a_closed_bus_is_no_host() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let _watcher = serve(&daemon, FakeWatcher { registered: true });
        let hosted = Arc::new(Hosted::new());
        let (session, kept) = (daemon.connect(), hosted.clone());
        let (done, ended) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(follow_on(&session, &kept));
        });
        reads(&hosted, true);
        drop(daemon);
        let ended = ended.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(ended.is_err());
        assert!(!hosted.shown());
    }
}
