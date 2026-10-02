//! The headless smoke run CI drives under `xvfb-run`: with
//! `STENO_SMOKE_SECONDS=<n>` the shell opens all three windows side by side,
//! waits that long, and exits 0 when the main window sent `page.ready` and
//! at least one snapshot reached it in reply, 1 otherwise; a value that is
//! not a positive number ends the run at once with 2. Screenshots of the
//! Xvfb root during the wait are the review evidence; the windows carry only
//! fixture data.

use std::{
    env, process,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
    time::Duration,
};

use tauri::{AppHandle, Manager};

use crate::windows::{self, BridgeWindow, Spec};

pub const SECONDS_VARIABLE: &str = "STENO_SMOKE_SECONDS";

/// What the run has seen so far; managed state, read by the timer thread.
/// Counts on every run, logs only on a smoke run.
#[derive(Debug, Default)]
pub struct Smoke {
    armed: AtomicBool,
    main_ready: AtomicBool,
    main_snapshots: AtomicUsize,
}

impl Smoke {
    /// Records a window's `page.ready`; the main window's is the one the
    /// run waits for.
    pub fn note_ready(&self, label: &str) {
        if self.armed.load(Ordering::SeqCst) {
            eprintln!("[steno-desktop] smoke: page.ready from {label}");
        }
        if label == BridgeWindow::Main.as_str() {
            self.main_ready.store(true, Ordering::SeqCst);
        }
    }

    /// Records a snapshot emitted to a window (`bridge::emit`); main's are
    /// the proof that a host answered its `page.ready`.
    pub fn note_snapshot(&self, label: &str) {
        if label == BridgeWindow::Main.as_str() {
            self.main_snapshots.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn outcome(&self) -> Outcome {
        match (
            self.main_ready.load(Ordering::SeqCst),
            self.main_snapshots.load(Ordering::SeqCst),
        ) {
            (false, _) => Outcome::NoPageReady,
            (true, 0) => Outcome::NoSnapshot,
            (true, snapshots) => Outcome::Ok { snapshots },
        }
    }
}

/// How a smoke run ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok {
        snapshots: usize,
    },
    /// The main window never reported its page mounted.
    NoPageReady,
    /// The page mounted but nothing answered it: no host is wired.
    NoSnapshot,
}

impl Outcome {
    pub fn exit_code(&self) -> i32 {
        i32::from(!matches!(self, Outcome::Ok { .. }))
    }

    pub fn message(&self, seconds: u64) -> String {
        match self {
            Outcome::Ok { snapshots } => {
                format!("ok, page.ready from main and {snapshots} snapshots reached it")
            }
            Outcome::NoPageReady => format!("FAILED, no page.ready from main in {seconds}s"),
            Outcome::NoSnapshot => {
                let built = if cfg!(feature = "fixture-host") {
                    ""
                } else {
                    " (built without the fixture-host feature)"
                };
                format!(
                    "FAILED, page.ready from main but no snapshot reached it in {seconds}s: \
                     no bridge host is wired{built}"
                )
            }
        }
    }
}

/// What `STENO_SMOKE_SECONDS` asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wait {
    /// The variable is unset: an ordinary run.
    NotASmokeRun,
    Seconds(u64),
    /// Set, but not a positive number; the raw value for the message.
    Invalid(String),
}

pub fn wait_from(value: Option<&str>) -> Wait {
    let Some(value) = value else {
        return Wait::NotASmokeRun;
    };
    match value.trim().parse::<u64>() {
        Ok(seconds) if seconds > 0 => Wait::Seconds(seconds),
        _ => Wait::Invalid(value.to_owned()),
    }
}

/// Arms the smoke run if `STENO_SMOKE_SECONDS` is set; a no-op otherwise.
pub fn arm(app: &AppHandle) {
    let seconds = match wait_from(env::var(SECONDS_VARIABLE).ok().as_deref()) {
        Wait::NotASmokeRun => return,
        Wait::Seconds(seconds) => seconds,
        Wait::Invalid(value) => {
            eprintln!(
                "[steno-desktop] smoke: {SECONDS_VARIABLE} must be a positive number, got {value:?}"
            );
            process::exit(2);
        }
    };
    app.state::<Smoke>().armed.store(true, Ordering::SeqCst);
    eprintln!("[steno-desktop] smoke: waiting {seconds}s for page.ready and a snapshot on main");
    // Side by side on the Xvfb screen so one root capture shows every window.
    let (main_width, main_height) = Spec::of(BridgeWindow::Main).size;
    let opened = windows::open(
        app,
        BridgeWindow::Settings,
        None,
        Some((main_width + 40.0, 0.0)),
    )
    .and_then(|_| {
        windows::open(
            app,
            BridgeWindow::Onboarding,
            None,
            Some((0.0, main_height + 60.0)),
        )
    });
    if let Err(error) = opened {
        eprintln!("[steno-desktop] smoke: opening the other windows failed: {error}");
    }
    let handle = app.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(seconds));
        let outcome = handle.state::<Smoke>().outcome();
        eprintln!("[steno-desktop] smoke: {}", outcome.message(seconds));
        handle.exit(outcome.exit_code());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_is_a_positive_number_of_seconds() {
        assert_eq!(wait_from(Some("10")), Wait::Seconds(10));
        assert_eq!(wait_from(Some(" 3 ")), Wait::Seconds(3));
        assert_eq!(wait_from(None), Wait::NotASmokeRun);
    }

    #[test]
    fn anything_else_is_invalid_and_keeps_the_raw_value() {
        assert_eq!(wait_from(Some("0")), Wait::Invalid("0".into()));
        assert_eq!(wait_from(Some("soon")), Wait::Invalid("soon".into()));
        assert_eq!(wait_from(Some("")), Wait::Invalid(String::new()));
        assert_eq!(wait_from(Some("-5")), Wait::Invalid("-5".into()));
    }

    #[test]
    fn only_the_main_window_counts() {
        let smoke = Smoke::default();
        smoke.note_ready("settings");
        smoke.note_ready("onboarding");
        smoke.note_snapshot("settings");
        assert_eq!(smoke.outcome(), Outcome::NoPageReady);
        smoke.note_ready("main");
        assert_eq!(smoke.outcome(), Outcome::NoSnapshot);
        smoke.note_snapshot("main");
        smoke.note_snapshot("main");
        assert_eq!(smoke.outcome(), Outcome::Ok { snapshots: 2 });
    }

    #[test]
    fn a_mounted_page_without_a_snapshot_fails_and_names_the_missing_host() {
        assert_eq!(Outcome::Ok { snapshots: 12 }.exit_code(), 0);
        assert_eq!(Outcome::NoPageReady.exit_code(), 1);
        assert_eq!(Outcome::NoSnapshot.exit_code(), 1);
        let message = Outcome::NoSnapshot.message(15);
        assert!(message.contains("no bridge host is wired"), "{message}");
        assert_eq!(
            message.contains("without the fixture-host feature"),
            !cfg!(feature = "fixture-host")
        );
        assert!(
            Outcome::NoPageReady
                .message(15)
                .contains("no page.ready from main in 15s")
        );
    }
}
