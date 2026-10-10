//! Linux: whether something on the desktop shows the tray's icon. The icon
//! is a status notifier item, which a host shows: KDE Plasma's panel,
//! Waybar, Omarchy's bar, GNOME with the `AppIndicator` extension. Hosts
//! register with a watcher, which owns `org.kde.StatusNotifierWatcher` on
//! the session bus and says through its `IsStatusNotifierHostRegistered`
//! property whether one did. Stock GNOME runs neither, and a watcher can
//! run with no host (KDE's `kded`, started for a KDE app under another
//! desktop), so the shell asks whether a watcher runs and whether a host
//! registered with it.
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
//! bus, after a reading that failed and once the bus has closed, the
//! shell counts no host: closing the main window then ends the app
//! instead of leaving it running unseen, the safe side. A failed reading
//! (a watcher that does not answer within five seconds, for one) is read
//! again ten seconds later, three times at most before the next change.
//! An `XEmbed`-only tray is not asked for and counts as none too.
//!
//! The property is asked of the watcher's unique name, so asking never
//! starts a watcher the bus could activate. A watcher whose object says it
//! has no such property (`UnknownProperty`, `InvalidArgs`,
//! `UnknownInterface`) counts as a host: its name is all there is to go
//! on. One that serves no object at the path yet (`UnknownObject`,
//! `UnknownMethod`), as a watcher that takes the name before it exports
//! its object would, fails the reading: it counts as none and is read
//! again.
//!
//! When the host goes while the main window is hidden, nothing on the
//! desktop shows the window: during a recording the bubble stays on
//! screen, with Stop, and a click on it opens the main window; otherwise
//! starting Steno again brings it forward (single instance).
//!
//! Swift: none needed; an `NSStatusItem` always shows in the menu bar.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use zbus::blocking::connection::Builder;
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type;

use crate::dbus::{owner_of, patient, proxy, spawn_client};

/// The name a status notifier watcher owns on the session bus, and the
/// interface it serves.
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

/// The watcher's object.
const WATCHER_PATH: &str = "/StatusNotifierWatcher";

/// The watcher's property that says whether a host registered.
const HOST_REGISTERED: &str = "IsStatusNotifierHostRegistered";

/// How long the follower waits to read again after a reading failed, and
/// how many times it does before the next change.
const RETRY: Duration = Duration::from_secs(10);
const RETRIES: u32 = 3;

/// The watcher's signals that change `HOST_REGISTERED`; KDE's watcher
/// sends both, the specification names only the first.
const HOST_SIGNALS: [&str; 2] = [
    "StatusNotifierHostRegistered",
    "StatusNotifierHostUnregistered",
];

/// What the session bus says about the watcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Watcher {
    /// No peer owns `WATCHER`.
    Absent,
    /// A peer owns it; `host` is its `HOST_REGISTERED`, none when the
    /// watcher has no such property.
    Present { host: Option<bool> },
}

/// Whether a host shows the icon, from a reading of the bus: a watcher
/// that says a host registered, or one without the property; no watcher,
/// a watcher with no host, or a failed reading is none.
fn shows_icon(reading: &zbus::Result<Watcher>) -> bool {
    match reading {
        Ok(Watcher::Present {
            host: Some(registered),
        }) => *registered,
        Ok(Watcher::Present { host: None }) => true,
        Ok(Watcher::Absent) | Err(_) => false,
    }
}

/// One reading: the watcher's owner (`GetNameOwner`), then its
/// `HOST_REGISTERED`, asked of that unique name. Only an answer that the
/// watcher has no such property is `host: None`; any other failure (no
/// answer in time, the owner gone, a value of another type) fails the
/// reading.
fn read(session: &Connection) -> zbus::Result<Watcher> {
    let Some(owner) = owner_of(session, WATCHER)? else {
        return Ok(Watcher::Absent);
    };
    let asked = proxy(session, owner.as_str(), WATCHER_PATH, WATCHER)?
        .get_property::<bool>(HOST_REGISTERED);
    let host = match asked {
        Ok(registered) => Some(registered),
        Err(zbus::Error::FDO(error)) if lacks_the_property(&error) => {
            tracing::debug!(%error, "the tray's watcher has no host property");
            None
        }
        Err(error) => return Err(error),
    };
    Ok(Watcher::Present { host })
}

/// Whether `error`, a watcher's answer to the property's `Get`, says its
/// object has no such property: zbus and sd-bus answer `UnknownProperty`,
/// `GDBus` and older Qt `InvalidArgs`, and an object without the watcher's
/// interface `UnknownInterface`. `UnknownObject` and `UnknownMethod` say
/// nothing is served at the path, so they are not this answer.
fn lacks_the_property(error: &zbus::fdo::Error) -> bool {
    use zbus::fdo::Error::{InvalidArgs, UnknownInterface, UnknownProperty};
    matches!(
        error,
        UnknownProperty(_) | InvalidArgs(_) | UnknownInterface(_)
    )
}

/// What the follower last found; no host until its first reading.
#[derive(Debug, Default)]
struct Hosted(AtomicU8);

/// `Hosted`'s values.
const UNREAD: u8 = 0;
const NO_HOST: u8 = 1;
const HOST: u8 = 2;

impl Hosted {
    const fn new() -> Self {
        Self(AtomicU8::new(UNREAD))
    }

    /// Whether a host shows the icon, as last read.
    fn shown(&self) -> bool {
        self.0.load(Ordering::SeqCst) == HOST
    }

    /// Keeps a reading; true when it changes what the shell counts, or is
    /// the first.
    fn note(&self, shown: bool) -> bool {
        let value = if shown { HOST } else { NO_HOST };
        self.0.swap(value, Ordering::SeqCst) != value
    }
}

/// The shell's reading.
static HOSTED: Hosted = Hosted::new();

/// Whether a host shows the icon, as the follower last read it
/// (`tray::has_host`); none until its first reading.
pub fn shown() -> bool {
    HOSTED.shown()
}

/// What the log says when the follower cannot start.
const UNFOLLOWED: &str = "no tray host is followed, so closing the main window quits Steno";

/// Starts the follower on the session bus, when there is one; without
/// one the shell counts no host.
pub fn follow() {
    if !crate::session_bus_named() {
        tracing::warn!("no session bus, so no tray host; closing the main window quits Steno");
        return;
    }
    spawn_client("steno-tray-host", UNFOLLOWED, || {
        follow_on(&patient(Builder::session()?)?, &HOSTED, RETRY)
    });
}

/// Keeps `hosted` up to date from `session` until the bus closes: a
/// reading now, and one after every change of the watcher's owner or of
/// its hosts; a reading that failed is read again `retry` later, `RETRIES`
/// times at most before the next change. Two threads forward the signals,
/// so neither stream waits while a reading runs; the readings run here,
/// one at a time, and a burst of signals makes one reading. An error is
/// one setting the follower up; the bus closing ends it with `Ok`, logged
/// here.
fn follow_on(session: &Connection, hosted: &Hosted, retry: Duration) -> zbus::Result<()> {
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
        // A failure here leaves the scope, which then waits on the owner
        // forwarder until the bus closes, so it is logged now.
        std::thread::Builder::new()
            .name("steno-tray-hosts".to_owned())
            .spawn_scoped(scope, forward(hosts, is_host_signal))
            .inspect_err(|error| tracing::warn!(%error, "{UNFOLLOWED}"))?;
        drop(nudge);
        let mut retried = 0;
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
            let next = if reading.is_err() && retried < RETRIES {
                retried += 1;
                nudged.recv_timeout(retry)
            } else {
                nudged.recv().map_err(RecvTimeoutError::from)
            };
            match next {
                Ok(()) => {
                    retried = 0;
                    while nudged.try_recv().is_ok() {}
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        hosted.note(false);
        tracing::warn!("the session bus closed; closing the main window quits Steno");
        Ok(())
    })
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
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    use super::*;
    use crate::dbus::tests::Daemon;

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

    /// Which answers to the property's `Get` say the watcher lacks it, so
    /// it counts as a host; every other one, a timeout and a vanished peer
    /// among them, fails the reading.
    #[test]
    fn only_an_answer_without_the_property_counts_as_a_host() {
        use zbus::fdo::Error;
        let lacks: &[fn(String) -> Error] = &[
            Error::UnknownProperty,
            Error::InvalidArgs,
            Error::UnknownInterface,
        ];
        let fails: &[fn(String) -> Error] = &[
            Error::UnknownMethod,
            Error::UnknownObject,
            Error::NoReply,
            Error::Timeout,
            Error::TimedOut,
            Error::ServiceUnknown,
            Error::NameHasNoOwner,
            Error::AccessDenied,
            Error::Failed,
            Error::Disconnected,
            Error::NotSupported,
        ];
        for (answers, lacking) in [(lacks, true), (fails, false)] {
            for answer in answers {
                let error = answer(String::new());
                assert_eq!(lacks_the_property(&error), lacking, "{error:?}");
            }
        }
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
    /// sent by hand (`Connection::emit_signal`). Its first `hangs` answers
    /// come only after `HANG`, longer than `follow_slowly`'s calls wait;
    /// `asked` counts the questions.
    #[derive(Default)]
    struct FakeWatcher {
        registered: bool,
        hangs: std::sync::Mutex<usize>,
        asked: Arc<AtomicUsize>,
    }

    impl FakeWatcher {
        fn new(registered: bool) -> Self {
            Self {
                registered,
                ..Self::default()
            }
        }
    }

    /// How long `FakeWatcher` takes to answer while it hangs, and how long
    /// `follow_slowly`'s calls wait.
    const HANG: Duration = Duration::from_millis(600);
    const SHORT_PATIENCE: Duration = Duration::from_millis(200);

    #[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
    impl FakeWatcher {
        #[zbus(property)]
        fn is_status_notifier_host_registered(&self) -> bool {
            self.asked.fetch_add(1, Ordering::SeqCst);
            let mut hangs = self.hangs.lock().unwrap();
            if *hangs > 0 {
                *hangs -= 1;
                drop(hangs);
                std::thread::sleep(HANG);
            }
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
        serve_at(daemon, WATCHER_PATH, watcher)
    }

    /// A peer on `daemon` that owns `WATCHER` and serves `watcher` at `path`.
    fn serve_at(
        daemon: &Daemon,
        path: &str,
        watcher: impl zbus::object_server::Interface,
    ) -> Connection {
        daemon
            .builder()
            .name(WATCHER)
            .unwrap()
            .serve_at(path, watcher)
            .unwrap()
            .build()
            .unwrap()
    }

    /// The follower on `daemon`, its reading in the returned `Hosted`.
    fn follow_daemon(daemon: &Daemon) -> Arc<Hosted> {
        spawn_follower(daemon.connect(), RETRY)
    }

    /// The follower on `daemon` over a connection whose calls wait
    /// `SHORT_PATIENCE`, reading again `retry` after a failure.
    fn follow_slowly(daemon: &Daemon, retry: Duration) -> Arc<Hosted> {
        let session = daemon
            .builder()
            .method_timeout(SHORT_PATIENCE)
            .build()
            .unwrap();
        spawn_follower(session, retry)
    }

    /// The follower on `session` on a thread of its own, reading again
    /// `retry` after a failure.
    fn spawn_follower(session: Connection, retry: Duration) -> Arc<Hosted> {
        let hosted = Arc::new(Hosted::new());
        let kept = hosted.clone();
        std::thread::spawn(move || follow_on(&session, &kept, retry));
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
        let watcher = serve(&daemon, FakeWatcher::new(true));
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
        let watcher = serve(&daemon, FakeWatcher::new(false));
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

    /// A watcher without the property counts as a host.
    #[test]
    fn a_watcher_without_the_property_counts_as_a_host() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let _watcher = serve(&daemon, BareWatcher { version: 0 });
        let hosted = follow_daemon(&daemon);
        reads(&hosted, true);
    }

    /// A watcher that owns the name but serves no object at its path yet
    /// is no host: zbus answers `UnknownObject`, which fails the reading.
    #[test]
    fn a_watcher_without_its_object_is_no_host() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let _watcher = serve_at(&daemon, "/Elsewhere", BareWatcher { version: 0 });
        let hosted = follow_daemon(&daemon);
        reads(&hosted, false);
    }

    /// A watcher that does not answer in time is no host, also after the
    /// readings again, although its late answer says one registered; a
    /// host signal after the retries are spent gets as many again.
    #[test]
    fn a_watcher_that_does_not_answer_is_no_host() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = Arc::default();
        let watcher = serve(
            &daemon,
            FakeWatcher {
                registered: true,
                hangs: usize::MAX.into(),
                asked: Arc::clone(&asked),
            },
        );
        let retry = Duration::from_millis(50);
        let hosted = follow_slowly(&daemon, retry);
        reads(&hosted, false);
        let settles_at = |questions: usize| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while asked.load(Ordering::SeqCst) < questions && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            // Time for one more question, which must not come.
            std::thread::sleep(HANG + retry * 2);
            assert_eq!(asked.load(Ordering::SeqCst), questions);
            assert!(!hosted.shown());
        };
        let round = RETRIES as usize + 1;
        settles_at(round);
        watcher
            .emit_signal(None::<&str>, WATCHER_PATH, WATCHER, HOST_SIGNALS[0], &())
            .unwrap();
        settles_at(2 * round);
    }

    /// A reading that failed is read again without a signal: a watcher
    /// that hangs once and then answers is a host.
    #[test]
    fn a_failed_reading_is_read_again() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let asked = Arc::default();
        let _watcher = serve(
            &daemon,
            FakeWatcher {
                registered: true,
                hangs: 1.into(),
                asked: Arc::clone(&asked),
            },
        );
        let hosted = follow_slowly(&daemon, Duration::from_millis(500));
        reads(&hosted, true);
        assert_eq!(asked.load(Ordering::SeqCst), 2);
    }

    /// The bus closing ends the follower, with no host.
    #[test]
    fn a_closed_bus_is_no_host() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let _watcher = serve(&daemon, FakeWatcher::new(true));
        let hosted = Arc::new(Hosted::new());
        let (session, kept) = (daemon.connect(), hosted.clone());
        let (done, ended) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(follow_on(&session, &kept, RETRY));
        });
        reads(&hosted, true);
        drop(daemon);
        let ended = ended.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(ended.is_ok(), "{ended:?}");
        assert!(!hosted.shown());
    }
}
