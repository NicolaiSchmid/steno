//! The headless smoke run CI drives under `xvfb-run`: with
//! `STENO_SMOKE_SECONDS=<n>` the shell opens all three windows side by side
//! and both floating panels under them, waits that long, hides the panels
//! again, and exits 0 when the main window sent `page.ready`, at least one
//! snapshot reached it in reply, the tray was built, and both panels were
//! visible at the size their page reported before the hide and hidden
//! after it; 1 otherwise; a value that
//! is not a positive number ends the run at once with 2. Screenshots of the
//! Xvfb root during the wait are the review evidence; the windows carry only
//! fixture data, the prompt names a made-up app.

use std::{
    collections::HashMap,
    env, process,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use tauri::{AppHandle, Manager};

use crate::{
    panel_geometry::same_size,
    panels::{self, Panel, PromptRequest},
    windows::{self, BridgeWindow, Spec},
};

pub const SECONDS_VARIABLE: &str = "STENO_SMOKE_SECONDS";

/// What the run has seen so far; managed state, read by the timer thread.
/// Counts on every run, logs only on a smoke run.
#[derive(Debug, Default)]
pub struct Smoke {
    armed: AtomicBool,
    main_ready: AtomicBool,
    main_snapshots: AtomicUsize,
    tray_built: AtomicBool,
    /// The size each panel's page last reported, checked against the
    /// window at the end.
    panel_sizes: Mutex<HashMap<String, (f64, f64)>>,
}

impl Smoke {
    /// The tray was built (`main.rs`); the run asserts it where the
    /// platform has one.
    pub fn note_tray(&self) {
        self.tray_built.store(true, Ordering::SeqCst);
    }

    /// A panel's page reported its size; logged on a smoke run so the
    /// screenshots can be read against the numbers, and kept so the end of
    /// the run can check the window took it.
    pub fn note_panel_size(&self, label: &str, size: (f64, f64)) {
        if self.armed.load(Ordering::SeqCst) {
            eprintln!(
                "[steno-desktop] smoke: the {label} panel measures {} by {}",
                size.0, size.1
            );
        }
        if let Ok(mut sizes) = self.panel_sizes.lock() {
            sizes.insert(label.to_owned(), size);
        }
    }

    fn panel_size(&self, label: &str) -> Option<(f64, f64)> {
        self.panel_sizes
            .lock()
            .ok()
            .and_then(|sizes| sizes.get(label).copied())
    }

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

    fn outcome(&self, panels: Result<(), String>) -> Outcome {
        match (
            self.main_ready.load(Ordering::SeqCst),
            self.main_snapshots.load(Ordering::SeqCst),
            self.tray_built.load(Ordering::SeqCst),
            panels,
        ) {
            (false, ..) => Outcome::NoPageReady,
            (true, 0, ..) => Outcome::NoSnapshot,
            (true, _, false, _) => Outcome::NoTray,
            (true, _, true, Err(problem)) => Outcome::PanelsFailed(problem),
            (true, snapshots, true, Ok(())) => Outcome::Ok { snapshots },
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
    /// `tray::build` failed (the error was logged at startup).
    NoTray,
    /// A panel did not show, take its page's size, or hide, as asked; the
    /// message says which.
    PanelsFailed(String),
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
            Outcome::NoTray => "FAILED, the tray icon was not built".to_owned(),
            Outcome::PanelsFailed(problem) => format!("FAILED, panels: {problem}"),
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
    // Both panels at once, under Settings, which the one rule never does
    // (`FloatingContent::resolve`); the run shows them to screenshot them.
    let prompt = PromptRequest {
        app_name: "Acme Meet".into(),
        seconds: 60,
    };
    for (panel, query, y) in [
        (Panel::Prompt, Some(prompt.query(None)), PANEL_PROMPT_Y),
        (Panel::Bubble, None, PANEL_BUBBLE_Y),
    ] {
        if let Err(error) = panels::show_at(app, panel, query.as_deref(), (PANELS_X, y)) {
            eprintln!(
                "[steno-desktop] smoke: opening the {} panel failed: {error}",
                panel.label()
            );
        }
    }
    let handle = app.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(seconds));
        let panels = check_panels(&handle);
        let outcome = handle.state::<Smoke>().outcome(panels);
        eprintln!("[steno-desktop] smoke: {}", outcome.message(seconds));
        handle.exit(outcome.exit_code());
    });
}

/// Where the smoke puts the panels: right of the main window, under
/// Settings; `smoke-linux.sh` crops the same places.
pub const PANELS_X: f64 = 1160.0;
pub const PANEL_PROMPT_Y: f64 = 700.0;
pub const PANEL_BUBBLE_Y: f64 = 800.0;

/// Both panels exist, are visible and have taken the size their page
/// reported, then hide on request and report hidden: the same `show`,
/// `resize` and `hide` the one rule drives, checked from outside.
fn check_panels(app: &AppHandle) -> Result<(), String> {
    let smoke = app.state::<Smoke>();
    for panel in Panel::ALL {
        let label = panel.label();
        let window = app
            .get_webview_window(label)
            .ok_or_else(|| format!("the {label} panel was not created"))?;
        if !window.is_visible().map_err(|error| error.to_string())? {
            return Err(format!("the {label} panel was not visible"));
        }
        let window_size = panels::logical_size(&window).map_err(|error| error.to_string())?;
        let reported = smoke
            .panel_size(label)
            .ok_or_else(|| format!("the {label} panel never reported its size"))?;
        if !same_size(window_size, reported) {
            return Err(format!(
                "the {label} panel measures {} by {} but its window is {} by {}",
                reported.0, reported.1, window_size.0, window_size.1
            ));
        }
        eprintln!(
            "[steno-desktop] smoke: the {label} window is {} by {}",
            window_size.0, window_size.1
        );
        panels::hide(app, panel).map_err(|error| format!("hiding {label}: {error}"))?;
        if window.is_visible().map_err(|error| error.to_string())? {
            return Err(format!("the {label} panel stayed visible after hide"));
        }
        eprintln!("[steno-desktop] smoke: the {label} panel showed and hid");
    }
    Ok(())
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
        assert_eq!(smoke.outcome(Ok(())), Outcome::NoPageReady);
        smoke.note_ready("main");
        assert_eq!(smoke.outcome(Ok(())), Outcome::NoSnapshot);
        smoke.note_snapshot("main");
        smoke.note_snapshot("main");
        assert_eq!(smoke.outcome(Ok(())), Outcome::NoTray);
        smoke.note_tray();
        assert_eq!(smoke.outcome(Ok(())), Outcome::Ok { snapshots: 2 });
    }

    #[test]
    fn the_tray_and_the_panels_are_checked_after_the_page() {
        let smoke = Smoke::default();
        smoke.note_ready("main");
        smoke.note_snapshot("main");
        smoke.note_tray();
        assert_eq!(
            smoke.outcome(Err("the bubble panel was not visible".into())),
            Outcome::PanelsFailed("the bubble panel was not visible".into())
        );
        assert_eq!(Outcome::NoTray.exit_code(), 1);
        assert_eq!(Outcome::PanelsFailed(String::new()).exit_code(), 1);
        assert!(Outcome::NoTray.message(5).contains("tray icon"));
        assert!(
            Outcome::PanelsFailed("x".into())
                .message(5)
                .contains("panels: x")
        );
        // The page's verdicts come first: a missing tray never masks them.
        let page_less = Smoke::default();
        page_less.note_tray();
        assert_eq!(page_less.outcome(Ok(())), Outcome::NoPageReady);
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
