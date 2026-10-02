//! The headless smoke run CI drives under `xvfb-run`: with
//! `STENO_SMOKE_SECONDS=<n>` the shell opens all three windows side by side,
//! waits that long, and exits 0 when the main window sent `page.ready`, 1
//! otherwise. Screenshots of the Xvfb root during the wait are the review
//! evidence; the windows carry only fixture data.

use std::{
    env,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use tauri::AppHandle;

use crate::windows::{self, Kind};

pub const SECONDS_VARIABLE: &str = "STENO_SMOKE_SECONDS";

static MAIN_READY: AtomicBool = AtomicBool::new(false);

/// Records a window's `page.ready`.
pub fn note_ready(label: &str) {
    if label == Kind::Main.label() {
        MAIN_READY.store(true, Ordering::SeqCst);
    }
}

/// The configured wait, when the run is a smoke run.
pub fn seconds_from(value: Option<&str>) -> Option<u64> {
    value?.trim().parse().ok().filter(|seconds| *seconds > 0)
}

/// Arms the smoke run if `STENO_SMOKE_SECONDS` is set; a no-op otherwise.
pub fn arm(app: &AppHandle) {
    let Some(seconds) = seconds_from(env::var(SECONDS_VARIABLE).ok().as_deref()) else {
        return;
    };
    eprintln!("[steno-desktop] smoke: waiting {seconds}s for page.ready from main");
    // Side by side on the Xvfb screen so one root capture shows every window.
    let main_width = Kind::Main.spec().size.0;
    let main_height = Kind::Main.spec().size.1;
    let opened = windows::open(app, Kind::Settings, None, Some((main_width + 40.0, 0.0)))
        .and_then(|_| windows::open(app, Kind::Onboarding, None, Some((0.0, main_height + 60.0))));
    if let Err(error) = opened {
        eprintln!("[steno-desktop] smoke: opening the other windows failed: {error}");
    }
    let handle = app.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(seconds));
        let ready = MAIN_READY.load(Ordering::SeqCst);
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
        assert_eq!(seconds_from(Some("10")), Some(10));
        assert_eq!(seconds_from(Some(" 3 ")), Some(3));
        assert_eq!(seconds_from(Some("0")), None);
        assert_eq!(seconds_from(Some("soon")), None);
        assert_eq!(seconds_from(None), None);
    }

    #[test]
    fn only_the_main_window_counts() {
        note_ready("settings");
        assert!(!MAIN_READY.load(Ordering::SeqCst));
        note_ready("main");
        assert!(MAIN_READY.load(Ordering::SeqCst));
    }
}
