//! The headless smoke run CI drives under `xvfb-run`: with
//! `STENO_SMOKE_SECONDS=<n>` the shell opens all three windows side by side,
//! waits that long, and exits 0 when the main window sent `page.ready`, 1
//! otherwise; a value that is not a positive number ends the run at once
//! with 2. Screenshots of the Xvfb root during the wait are the review
//! evidence; the windows carry only fixture data.

use std::{
    env, process,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use tauri::{AppHandle, Manager};

use crate::windows::{self, BridgeWindow};

pub const SECONDS_VARIABLE: &str = "STENO_SMOKE_SECONDS";

/// What the run has seen so far; managed state, read by the timer thread.
#[derive(Debug, Default)]
pub struct Smoke {
    main_ready: AtomicBool,
}

impl Smoke {
    /// Records a window's `page.ready`; the main window's is the one the
    /// run waits for.
    pub fn note_ready(&self, label: &str) {
        eprintln!("[steno-desktop] page.ready from {label}");
        if label == BridgeWindow::Main.label() {
            self.main_ready.store(true, Ordering::SeqCst);
        }
    }

    fn main_ready(&self) -> bool {
        self.main_ready.load(Ordering::SeqCst)
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
    eprintln!("[steno-desktop] smoke: waiting {seconds}s for page.ready from main");
    // Side by side on the Xvfb screen so one root capture shows every window.
    let main_width = BridgeWindow::Main.spec().size.0;
    let main_height = BridgeWindow::Main.spec().size.1;
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
        let ready = handle.state::<Smoke>().main_ready();
        if ready {
            eprintln!("[steno-desktop] smoke: ok, page.ready arrived");
        } else {
            eprintln!("[steno-desktop] smoke: FAILED, no page.ready from main in {seconds}s");
        }
        handle.exit(i32::from(!ready));
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
        assert!(!smoke.main_ready());
        smoke.note_ready("main");
        assert!(smoke.main_ready());
    }
}
