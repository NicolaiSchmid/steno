//! Linux: whether a status notifier host (Plasma's panel, Waybar, GNOME's
//! `AppIndicator` extension) shows the tray's icon. A thread (`follow`)
//! reads the watcher's owner and `IsStatusNotifierHostRegistered` at launch
//! and on every change, and keeps the answer in `HOSTED` for the main
//! window's close: with a host it hides, without one it quits. Until a
//! reading succeeds the shell counts no host, the safe side.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use zbus::blocking::connection::Builder;
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type;

use crate::dbus::{owner_of, patient, proxy, spawn_client};

/// The name a status notifier watcher owns, and the interface it serves.
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

const WATCHER_PATH: &str = "/StatusNotifierWatcher";

const HOST_REGISTERED: &str = "IsStatusNotifierHostRegistered";

/// How long the follower waits to read again after a failed reading (one
/// that found an object), and how many times before the next change.
const RETRY: Duration = Duration::from_secs(10);
const RETRIES: u32 = 3;

/// The watcher's signals that change `HOST_REGISTERED`. KDE's watcher
/// sends neither; its property is always true.
const HOST_SIGNALS: [&str; 2] = [
    "StatusNotifierHostRegistered",
    "StatusNotifierHostUnregistered",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Watcher {
    /// No peer owns `WATCHER`.
    Absent,
    /// A peer owns it; `host` is its `HOST_REGISTERED`, none when the
    /// watcher lacks the property.
    Present { host: Option<bool> },
}

/// Whether a host shows the icon: a watcher that says a host registered,
/// or one without the property.
fn shows_icon(reading: &zbus::Result<Watcher>) -> bool {
    match reading {
        Ok(Watcher::Present {
            host: Some(registered),
        }) => *registered,
        Ok(Watcher::Present { host: None }) => true,
        Ok(Watcher::Absent) | Err(_) => false,
    }
}

/// One reading: the watcher's owner, then its `HOST_REGISTERED` asked of
/// that unique name, so asking never activates a watcher. Only a
/// missing-property answer is `host: None`; other failures fail the reading.
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

/// Whether `error`, the answer to the property's `Get`, says the object
/// lacks the property (`UnknownProperty`, `InvalidArgs` or
/// `UnknownInterface`, depending on the bus library).
fn lacks_the_property(error: &zbus::fdo::Error) -> bool {
    use zbus::fdo::Error::{InvalidArgs, UnknownInterface, UnknownProperty};
    matches!(
        error,
        UnknownProperty(_) | InvalidArgs(_) | UnknownInterface(_)
    )
}

/// Whether `error` says the name is owned but nothing is served at its
/// path yet. Not read again before the next change: the tray library
/// registers the icon again only when the name changes owner.
fn serves_nothing(error: &zbus::Error) -> bool {
    use zbus::fdo::Error::{UnknownMethod, UnknownObject};
    let zbus::Error::FDO(error) = error else {
        return false;
    };
    matches!(**error, UnknownObject(_) | UnknownMethod(_))
}

#[derive(Debug, Default)]
struct Hosted(AtomicU8);

const UNREAD: u8 = 0;
const NO_HOST: u8 = 1;
const HOST: u8 = 2;

impl Hosted {
    const fn new() -> Self {
        Self(AtomicU8::new(UNREAD))
    }

    fn shown(&self) -> bool {
        self.0.load(Ordering::SeqCst) == HOST
    }

    /// Keeps a reading; true when it changes the answer or is the first.
    fn note(&self, shown: bool) -> bool {
        let value = if shown { HOST } else { NO_HOST };
        self.0.swap(value, Ordering::SeqCst) != value
    }
}

static HOSTED: Hosted = Hosted::new();

/// Whether a host shows the icon, as last read; none before the first
/// reading.
pub fn shown() -> bool {
    HOSTED.shown()
}

const UNFOLLOWED: &str = "no tray host is followed, so closing the main window quits Steno";

/// Starts the follower on the session bus; without one the shell counts
/// no host.
pub fn follow() {
    if !crate::session_bus_named() {
        tracing::warn!("no session bus, so no tray host; closing the main window quits Steno");
        return;
    }
    spawn_client("steno-tray-host", UNFOLLOWED, || {
        follow_on(&patient(Builder::session()?)?, &HOSTED, RETRY)
    });
}

/// Keeps `hosted` up to date from `session` until the bus closes: reads now
/// and after every change of the watcher's owner or hosts, and retries a
/// failed reading `retry` later up to `RETRIES` times. Two threads forward
/// the signals; the readings run here one at a time, so a burst makes one
/// reading.
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
        // Logged now: a failure leaves the scope waiting on the owner
        // forwarder until the bus closes.
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
            let read_again = matches!(&reading, Err(error) if !serves_nothing(error));
            let next = if read_again && retried < RETRIES {
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

    #[test]
    fn a_host_shows_the_icon_only_where_the_watcher_says_one_registered() {
        let present = |host| Ok(Watcher::Present { host });
        assert!(shows_icon(&present(Some(true))));
        assert!(!shows_icon(&present(Some(false))));
        assert!(shows_icon(&present(None)));
        assert!(!shows_icon(&Ok(Watcher::Absent)));
        assert!(!shows_icon(&Err(zbus::Error::Failure("no bus".to_owned()))));
    }

    /// Which `Get` answers count as a missing property, hence a host.
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

    /// A fake watcher: the property, and signals sent by hand. Its first
    /// `hangs` answers come only after `HANG`; `asked` counts the questions.
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

    /// How long `FakeWatcher` hangs; longer than `follow_slowly` waits.
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

    /// A watcher without the host property.
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

    fn follow_daemon(daemon: &Daemon) -> Arc<Hosted> {
        spawn_follower(daemon.connect(), RETRY)
    }

    /// The follower on `daemon` over a connection with short call timeouts.
    fn follow_slowly(daemon: &Daemon, retry: Duration) -> Arc<Hosted> {
        let session = daemon
            .builder()
            .method_timeout(SHORT_PATIENCE)
            .build()
            .unwrap();
        spawn_follower(session, retry)
    }

    fn spawn_follower(session: Connection, retry: Duration) -> Arc<Hosted> {
        let hosted = Arc::new(Hosted::new());
        let kept = hosted.clone();
        std::thread::spawn(move || follow_on(&session, &kept, retry));
        hosted
    }

    /// Waits up to ten seconds until `hosted` reads `shown`.
    fn reads(hosted: &Hosted, shown: bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let wanted = if shown { HOST } else { NO_HOST };
        while hosted.0.load(Ordering::SeqCst) != wanted && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(hosted.shown(), shown);
        assert_ne!(hosted.0.load(Ordering::SeqCst), UNREAD);
    }

    /// The host follows the watcher's arrival and departure.
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

    /// A watcher with no host counts once a host registers, until it goes.
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

    /// A watcher that serves no object yet is no host, until the next
    /// change.
    #[test]
    fn a_watcher_without_its_object_is_no_host() {
        let Some(daemon) = Daemon::start() else {
            return;
        };
        let watcher = serve_at(&daemon, "/Elsewhere", BareWatcher { version: 0 });
        let retry = Duration::from_millis(300);
        let hosted = spawn_follower(daemon.connect(), retry);
        reads(&hosted, false);
        let asked = Arc::default();
        let exported = FakeWatcher {
            registered: true,
            hangs: 0.into(),
            asked: Arc::clone(&asked),
        };
        assert!(watcher.object_server().at(WATCHER_PATH, exported).unwrap());
        std::thread::sleep(retry * 3);
        assert_eq!(asked.load(Ordering::SeqCst), 0);
        assert!(!hosted.shown());
        watcher
            .emit_signal(None::<&str>, WATCHER_PATH, WATCHER, HOST_SIGNALS[0], &())
            .unwrap();
        reads(&hosted, true);
    }

    /// A watcher that never answers in time is no host, after the retries;
    /// a host signal earns a fresh set of retries.
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

    /// A failed reading is retried without a signal.
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
