//! The smoke run CI drives (`smoke-linux.sh`, `smoke-macos.sh`): with
//! `STENO_SMOKE_SECONDS=<n>` the shell opens every window and both panels,
//! asks main for a meeting before its page mounts, waits, then checks the
//! page handshake, the tray, the panels and the window close rules. Exits
//! 0 when all pass, 1 otherwise, 2 for an invalid value.

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

use serde_json::Value;
use steno_bridge::{BridgeTopic, WindowParams};
use steno_core::json::parse_uuid;
use tauri::{AppHandle, LogicalSize, Manager, WebviewWindow};

use crate::{
    panel_geometry::same_size,
    panels::{self, Panel, PromptRequest},
    windows::{self, BridgeWindow, RequestField, Spec},
};

pub const SECONDS_VARIABLE: &str = "STENO_SMOKE_SECONDS";

/// What the run has seen so far; managed state, read by the timer thread.
#[derive(Debug, Default)]
pub struct Smoke {
    armed: AtomicBool,
    main_ready: AtomicBool,
    main_snapshots: AtomicUsize,
    /// The meeting asked of main reached it after its `page.ready`.
    request_delivered: AtomicBool,
    /// How many Settings pages sent `page.ready`.
    settings_ready: AtomicUsize,
    /// How many times the host heard that onboarding closed.
    onboarding_closed: AtomicUsize,
    tray_built: AtomicBool,
    /// The size each panel's page last reported.
    panel_sizes: Mutex<HashMap<Panel, (f64, f64)>>,
}

impl Smoke {
    /// The tray was built.
    pub fn note_tray(&self) {
        self.tray_built.store(true, Ordering::SeqCst);
    }

    /// The host heard that onboarding closed.
    pub fn note_onboarding_closed(&self) {
        self.onboarding_closed.fetch_add(1, Ordering::SeqCst);
    }

    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::SeqCst)
    }

    /// A panel's page reported its size; logged and kept for the end check.
    pub fn note_panel_size(&self, panel: Panel, size: (f64, f64)) {
        if self.armed.load(Ordering::SeqCst) {
            stderr_line!(
                "[steno-desktop] smoke: the {} panel measures {} by {}",
                panel.label(),
                size.0,
                size.1
            );
        }
        if let Ok(mut sizes) = self.panel_sizes.lock() {
            sizes.insert(panel, size);
        }
    }

    fn panel_size(&self, panel: Panel) -> Option<(f64, f64)> {
        self.panel_sizes
            .lock()
            .ok()
            .and_then(|sizes| sizes.get(&panel).copied())
    }

    /// Records a window's `page.ready`.
    pub fn note_ready(&self, label: &str) {
        if self.armed.load(Ordering::SeqCst) {
            stderr_line!("[steno-desktop] smoke: page.ready from {label}");
        }
        if label == BridgeWindow::Main.as_str() {
            self.main_ready.store(true, Ordering::SeqCst);
        }
        if label == BridgeWindow::Settings.as_str() {
            self.settings_ready.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Records a snapshot emitted to a window: main's prove the host
    /// answered, and an `app` snapshot carrying `SMOKE_MEETING` after its
    /// `page.ready` is the delivered request.
    pub fn note_snapshot(&self, label: &str, topic: BridgeTopic, payload: &Value) {
        if label != BridgeWindow::Main.as_str() {
            return;
        }
        self.main_snapshots.fetch_add(1, Ordering::SeqCst);
        if topic == BridgeTopic::App
            && payload[RequestField::MeetingId.as_str()] == SMOKE_MEETING
            && self.main_ready.load(Ordering::SeqCst)
        {
            self.request_delivered.store(true, Ordering::SeqCst);
        }
    }

    /// The run's verdict, given the panels' and windows' `checks`.
    fn outcome(&self, checks: Result<(), Outcome>) -> Outcome {
        match (
            self.main_ready.load(Ordering::SeqCst),
            self.main_snapshots.load(Ordering::SeqCst),
            self.request_delivered.load(Ordering::SeqCst),
            self.tray_built.load(Ordering::SeqCst),
            checks,
        ) {
            (false, ..) => Outcome::NoPageReady,
            (true, 0, ..) => Outcome::NoSnapshot,
            (true, _, false, ..) => Outcome::RequestLost,
            (true, _, true, false, _) => Outcome::NoTray,
            (true, _, true, true, Err(failed)) => failed,
            (true, snapshots, true, true, Ok(())) => Outcome::Ok { snapshots },
        }
    }
}

/// The meeting the run asks main for before its page mounts.
pub const SMOKE_MEETING: &str = "00000000-0000-0000-0000-00000000005E";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok {
        snapshots: usize,
    },
    /// The main window never reported its page mounted.
    NoPageReady,
    /// The page mounted but the host published nothing to it.
    NoSnapshot,
    /// The early meeting request never reached main after `page.ready`.
    RequestLost,
    /// `tray::build` failed.
    NoTray,
    /// A panel check failed; the message says which.
    PanelsFailed(String),
    /// A window close check failed; the message says which.
    WindowsFailed(String),
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
            Outcome::RequestLost => format!(
                "FAILED, the meeting asked of main before its page mounted did not reach it \
                 after its page.ready in {seconds}s"
            ),
            Outcome::PanelsFailed(problem) => format!("FAILED, panels: {problem}"),
            Outcome::WindowsFailed(problem) => format!("FAILED, windows: {problem}"),
            Outcome::NoSnapshot => format!(
                "FAILED, page.ready from main but no snapshot reached it in {seconds}s: \
                 the bridge host did not answer"
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wait {
    /// The variable is unset: an ordinary run.
    NotASmokeRun,
    Seconds(u64),
    /// Set, but not a positive number.
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

pub fn arm(app: &AppHandle) {
    let seconds = match wait_from(env::var(SECONDS_VARIABLE).ok().as_deref()) {
        Wait::NotASmokeRun => return,
        Wait::Seconds(seconds) => seconds,
        Wait::Invalid(value) => {
            stderr_line!(
                "[steno-desktop] smoke: {SECONDS_VARIABLE} must be a positive number, got {value:?}"
            );
            steno_services::flush_logs();
            process::exit(2);
        }
    };
    app.state::<Smoke>().armed.store(true, Ordering::SeqCst);
    stderr_line!("[steno-desktop] smoke: waiting {seconds}s for page.ready and a snapshot on main");
    // Side by side so one root capture shows every window.
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
        stderr_line!("[steno-desktop] smoke: opening the other windows failed: {error}");
    }
    // Asked before main's page mounts, as a deep link at a cold launch.
    let meeting = WindowParams {
        window: BridgeWindow::Main,
        section: None,
        meeting_id: parse_uuid(SMOKE_MEETING),
    };
    if let Err(error) = windows::open_requested(app, &meeting) {
        stderr_line!("[steno-desktop] smoke: asking main for a meeting failed: {error}");
    }
    // Both panels at once, which the app never does, to screenshot them.
    let prompt = PromptRequest {
        app_name: "Acme Meet".into(),
        seconds: 60,
    };
    for (panel, query, y) in [
        (Panel::Prompt, Some(prompt.query(None)), PANEL_PROMPT_Y),
        (Panel::Bubble, None, PANEL_BUBBLE_Y),
    ] {
        if let Err(error) = panels::show_at(app, panel, query.as_deref(), (PANELS_X, y)) {
            stderr_line!(
                "[steno-desktop] smoke: opening the {} panel failed: {error}",
                panel.label()
            );
        }
    }
    let handle = app.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(seconds));
        let checks = check_panels(&handle)
            .map_err(Outcome::PanelsFailed)
            .and_then(|()| {
                check_main_hides(&handle)
                    .and_then(|()| check_settings_reopens(&handle))
                    .and_then(|()| check_onboarding_is_kept(&handle))
                    .map_err(Outcome::WindowsFailed)
            });
        let outcome = handle.state::<Smoke>().outcome(checks);
        stderr_line!("[steno-desktop] smoke: {}", outcome.message(seconds));
        handle.exit(outcome.exit_code());
    });
}

/// Where the smoke puts the panels; `smoke-linux.sh` crops the same places.
pub const PANELS_X: f64 = 1160.0;
pub const PANEL_PROMPT_Y: f64 = 700.0;
pub const PANEL_BUBBLE_Y: f64 = 800.0;

/// Both panels show at their page's size, hold it, the prompt takes a
/// second request, and both hide on request.
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
            .panel_size(panel)
            .ok_or_else(|| format!("the {label} panel never reported its size"))?;
        if !same_size(window_size, reported) {
            return Err(format!(
                "the {label} panel measures {} by {} but its window is {} by {}",
                reported.0, reported.1, window_size.0, window_size.1
            ));
        }
        stderr_line!(
            "[steno-desktop] smoke: the {label} window is {} by {}",
            window_size.0,
            window_size.1
        );
        keeps_its_size(&window, label, window_size)?;
        if panel == Panel::Prompt {
            takes_a_second_prompt(app, &window)?;
        }
        panels::hide(app, panel).map_err(|error| format!("hiding {label}: {error}"))?;
        if window.is_visible().map_err(|error| error.to_string())? {
            return Err(format!("the {label} panel stayed visible after hide"));
        }
        stderr_line!("[steno-desktop] smoke: the {label} panel showed and hid");
    }
    Ok(())
}

/// A second prompt navigates the prompt's existing window.
fn takes_a_second_prompt(app: &AppHandle, window: &WebviewWindow) -> Result<(), String> {
    let second = PromptRequest {
        app_name: "Acme Notes".into(),
        seconds: 30,
    };
    let query = second.query(None);
    panels::show_at(app, Panel::Prompt, Some(&query), (PANELS_X, PANEL_PROMPT_Y))
        .map_err(|error| format!("raising a second prompt: {error}"))?;
    thread::sleep(Duration::from_millis(500));
    let url = window.url().map_err(|error| error.to_string())?;
    let wanted = Panel::Prompt.fragment(Some(&query));
    if url.fragment() != Some(wanted.as_str()) {
        return Err(format!(
            "the second prompt did not reach the prompt's window: it shows {:?}, not {wanted:?}",
            url.fragment()
        ));
    }
    stderr_line!("[steno-desktop] smoke: the prompt's window took a second prompt");
    Ok(())
}

/// Closing main while a tray stands hides it rather than destroying it.
fn check_main_hides(app: &AppHandle) -> Result<(), String> {
    let main = app
        .get_webview_window(BridgeWindow::Main.as_str())
        .ok_or("the main window was gone before the close")?;
    main.close().map_err(|error| error.to_string())?;
    thread::sleep(Duration::from_millis(1000));
    let main = app
        .get_webview_window(BridgeWindow::Main.as_str())
        .ok_or("closing the main window destroyed it")?;
    if main.is_visible().map_err(|error| error.to_string())? {
        return Err("the main window stayed visible after a close".into());
    }
    stderr_line!("[steno-desktop] smoke: closing main hid it");
    Ok(())
}

/// On Linux, a closed Settings window is kept and reopens on a section
/// with a fresh page; elsewhere a close destroys it.
fn check_settings_reopens(app: &AppHandle) -> Result<(), String> {
    let label = BridgeWindow::Settings.as_str();
    if !windows::keeps_on_close(label) {
        return Ok(());
    }
    let mounted = app.state::<Smoke>().settings_ready.load(Ordering::SeqCst);
    let kept = close_twice(app, label, "Settings")?;
    let section = steno_bridge::SettingsSection::Export;
    let params = WindowParams {
        window: BridgeWindow::Settings,
        section: Some(section),
        meeting_id: None,
    };
    windows::open_requested(app, &params)
        .map_err(|error| format!("opening Settings again: {error}"))?;
    // A slow debug build may take a while to mount the page.
    let smoke = app.state::<Smoke>();
    for _ in 0..50 {
        if smoke.settings_ready.load(Ordering::SeqCst) > mounted {
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    if !kept.is_visible().map_err(|error| error.to_string())? {
        return Err("Settings opened again stayed hidden".into());
    }
    let wanted = format!("/settings?section={section}");
    let url = kept.url().map_err(|error| error.to_string())?;
    if url.fragment() != Some(wanted.as_str()) {
        return Err(format!(
            "Settings opened again shows {:?}, not {wanted:?}",
            url.fragment()
        ));
    }
    if smoke.settings_ready.load(Ordering::SeqCst) <= mounted {
        return Err("Settings opened again mounted no new page in 10 s".into());
    }
    stderr_line!(
        "[steno-desktop] smoke: closing Settings kept it, and it opened again on {section}"
    );
    Ok(())
}

/// On Linux, a closed onboarding window is kept and the host hears of the
/// close once; elsewhere a close destroys it.
fn check_onboarding_is_kept(app: &AppHandle) -> Result<(), String> {
    let label = BridgeWindow::Onboarding.as_str();
    if !windows::keeps_on_close(label) {
        return Ok(());
    }
    let smoke = app.state::<Smoke>();
    let told = smoke.onboarding_closed.load(Ordering::SeqCst);
    close_twice(app, label, "onboarding")?;
    let times = smoke.onboarding_closed.load(Ordering::SeqCst) - told;
    if times != 1 {
        return Err(format!(
            "closing onboarding twice told the host {times} times, not once"
        ));
    }
    stderr_line!("[steno-desktop] smoke: closing onboarding kept it and told the host once");
    Ok(())
}

/// Closes the window of `label` (`name` in messages) twice and returns it
/// once kept, still there and hidden.
fn close_twice(app: &AppHandle, label: &str, name: &str) -> Result<WebviewWindow, String> {
    let window = app
        .get_webview_window(label)
        .ok_or_else(|| format!("{name} was gone before the close"))?;
    window.close().map_err(|error| error.to_string())?;
    thread::sleep(Duration::from_millis(1000));
    let kept = app
        .get_webview_window(label)
        .ok_or_else(|| format!("closing {name} destroyed it"))?;
    if kept.is_visible().map_err(|error| error.to_string())? {
        return Err(format!("{name} stayed visible after a close"));
    }
    kept.close().map_err(|error| error.to_string())?;
    thread::sleep(Duration::from_millis(300));
    Ok(kept)
}

/// A panel keeps its page's size: on Linux a resize request is refused by
/// the pin, elsewhere the window is not resizable.
fn keeps_its_size(window: &WebviewWindow, label: &str, size: (f64, f64)) -> Result<(), String> {
    let resizable = window.is_resizable().map_err(|error| error.to_string())?;
    if resizable != panels::RESIZABLE {
        let not = if resizable { "" } else { " not" };
        return Err(format!("the {label} panel is{not} resizable"));
    }
    if !resizable {
        stderr_line!("[steno-desktop] smoke: the {label} panel cannot be resized");
        return Ok(());
    }
    window
        .set_size(LogicalSize::new(size.0 + 40.0, size.1 + 40.0))
        .map_err(|error| error.to_string())?;
    thread::sleep(Duration::from_millis(500));
    let after = panels::logical_size(window).map_err(|error| error.to_string())?;
    if !same_size(after, size) {
        return Err(format!(
            "the {label} panel grew from {} by {} to {} by {} when asked",
            size.0, size.1, after.0, after.1
        ));
    }
    stderr_line!("[steno-desktop] smoke: the {label} panel kept its size");
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

    fn app_requesting(meeting: &str) -> Value {
        serde_json::json!({ "version": "1", "requestedMeetingID": meeting })
    }

    #[test]
    fn only_the_main_window_counts() {
        let smoke = Smoke::default();
        smoke.note_ready("settings");
        smoke.note_ready("onboarding");
        smoke.note_snapshot("settings", BridgeTopic::App, &app_requesting(SMOKE_MEETING));
        assert_eq!(smoke.outcome(Ok(())), Outcome::NoPageReady);
        smoke.note_ready("main");
        assert_eq!(smoke.outcome(Ok(())), Outcome::NoSnapshot);
        smoke.note_snapshot("main", BridgeTopic::Recording, &Value::Null);
        assert_eq!(smoke.outcome(Ok(())), Outcome::RequestLost);
        smoke.note_snapshot("main", BridgeTopic::App, &app_requesting(SMOKE_MEETING));
        assert_eq!(smoke.outcome(Ok(())), Outcome::NoTray);
        smoke.note_tray();
        assert_eq!(smoke.outcome(Ok(())), Outcome::Ok { snapshots: 2 });
    }

    /// The meeting counts only when it reaches main after its `page.ready`.
    #[test]
    fn the_early_meeting_counts_only_after_page_ready() {
        let smoke = Smoke::default();
        smoke.note_tray();
        smoke.note_snapshot("main", BridgeTopic::App, &app_requesting(SMOKE_MEETING));
        smoke.note_ready("main");
        smoke.note_snapshot("main", BridgeTopic::App, &app_requesting("other"));
        smoke.note_snapshot(
            "main",
            BridgeTopic::MeetingsList,
            &app_requesting(SMOKE_MEETING),
        );
        assert_eq!(smoke.outcome(Ok(())), Outcome::RequestLost);
        assert!(
            Outcome::RequestLost
                .message(15)
                .contains("after its page.ready")
        );
        smoke.note_snapshot("main", BridgeTopic::App, &app_requesting(SMOKE_MEETING));
        assert_eq!(smoke.outcome(Ok(())), Outcome::Ok { snapshots: 4 });
        let id = parse_uuid(SMOKE_MEETING).unwrap();
        assert_eq!(windows::Request::meeting(&id).value, SMOKE_MEETING);
    }

    #[test]
    fn the_tray_and_the_panels_are_checked_after_the_page() {
        let smoke = Smoke::default();
        smoke.note_ready("main");
        smoke.note_snapshot("main", BridgeTopic::App, &app_requesting(SMOKE_MEETING));
        smoke.note_tray();
        let hidden = Outcome::PanelsFailed("the bubble panel was not visible".into());
        assert_eq!(smoke.outcome(Err(hidden.clone())), hidden);
        let destroyed = Outcome::WindowsFailed("closing Settings destroyed it".into());
        assert_eq!(smoke.outcome(Err(destroyed.clone())), destroyed);
        assert_eq!(Outcome::NoTray.exit_code(), 1);
        assert_eq!(Outcome::RequestLost.exit_code(), 1);
        assert_eq!(Outcome::PanelsFailed(String::new()).exit_code(), 1);
        assert_eq!(Outcome::WindowsFailed(String::new()).exit_code(), 1);
        assert!(Outcome::NoTray.message(5).contains("tray icon"));
        assert!(
            Outcome::PanelsFailed("x".into())
                .message(5)
                .contains("panels: x")
        );
        assert!(
            Outcome::WindowsFailed("x".into())
                .message(5)
                .contains("windows: x")
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
        assert!(
            message.contains("the bridge host did not answer"),
            "{message}"
        );
        assert!(
            Outcome::NoPageReady
                .message(15)
                .contains("no page.ready from main in 15s")
        );
    }
}
