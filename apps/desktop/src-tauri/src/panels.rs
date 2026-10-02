//! The floating panels: the recording bubble and the detection prompt as
//! two small webviews at the web app's `#/panel/bubble` and
//! `#/panel/prompt` routes, undecorated, transparent, always on top, on
//! every space, never focused. On macOS they become non-activating
//! `NSPanel`s through `tauri-nspanel`, as the Swift `FloatingPanel` is; on
//! Linux and Windows they are always-on-top windows.
//!
//! One rule decides what shows (`FloatingContent::resolve`): a busy
//! recorder wins, else a prompt if one is pending, else nothing. The shell
//! reads the recorder off the `recording` snapshots passing through
//! `bridge::emit`; the host raises and clears the prompt
//! (`set_prompt`, `WP6b` wires the detection controller to it).
//!
//! Both panels hang from one anchor, the top-centre point of the frame, so
//! the prompt turns into the bubble without moving; the user drags a
//! panel by its background (`data-tauri-drag-region` in the page), the
//! anchor follows and is saved. A saved anchor on a screen that is gone
//! falls back to the default: top centre of the main screen, 8 pt under
//! its top edge. The page measures itself and reports its size through
//! `panel_call("resize")`; the shell sizes the window from that, as the
//! Swift root reported through `contentSizeDidChange`.
//!
//! Swift: `FloatingPanel.swift`, `FloatingPanelModel.swift`,
//! `FloatingContent.swift`.

use std::{collections::HashMap, fs, path::PathBuf, sync::Mutex};

use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, PhysicalPosition, Url, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder, webview::NewWindowResponse,
};

use crate::{navigation, recording::RecordingState};

/// `Theme.Space.sm`: the default anchor's distance from the screen's top.
pub const DEFAULT_TOP_INSET: f64 = 8.0;

/// The two panels; the raw value is the window label and the route's
/// last segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    Bubble,
    Prompt,
}

impl Panel {
    pub const ALL: [Panel; 2] = [Self::Bubble, Self::Prompt];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Bubble => "bubble",
            Self::Prompt => "prompt",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|panel| panel.label() == label)
    }

    pub const fn route(self) -> &'static str {
        match self {
            Self::Bubble => "#/panel/bubble",
            Self::Prompt => "#/panel/prompt",
        }
    }

    /// The size before the page has measured: the Swift maximum width
    /// (`PanelMetrics.bubbleMaxWidth`, `promptMaxWidth`), so the pill lays
    /// out at its intrinsic width and the window then shrinks to what the
    /// page reports; the heights are the one-row bubble's and the prompt's.
    pub const fn initial_size(self) -> (f64, f64) {
        match self {
            Self::Bubble => (480.0, 40.0),
            Self::Prompt => (480.0, 56.0),
        }
    }

    /// `index.html#/panel/<label>?<query>`.
    pub fn start_path(self, query: Option<&str>) -> String {
        match query {
            Some(query) if !query.is_empty() => format!("index.html{}?{query}", self.route()),
            _ => format!("index.html{}", self.route()),
        }
    }
}

/// What the detection prompt asks: which app opened the microphone and
/// how long the prompt stays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptRequest {
    pub app_name: String,
    pub seconds: u64,
}

impl PromptRequest {
    /// The prompt's query: `app=<name>&seconds=<n>`, form-encoded.
    pub fn query(&self) -> String {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("app", &self.app_name)
            .append_pair("seconds", &self.seconds.to_string())
            .finish()
    }
}

/// What the one floating surface shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FloatingContent {
    Prompt(PromptRequest),
    Bubble,
}

impl FloatingContent {
    /// The one rule: a busy recorder wins, else a prompt, else hidden.
    pub fn resolve(prompt: Option<&PromptRequest>, recording: RecordingState) -> Option<Self> {
        if recording.is_busy() {
            return Some(Self::Bubble);
        }
        prompt.cloned().map(Self::Prompt)
    }

    pub const fn panel(&self) -> Panel {
        match self {
            Self::Prompt(_) => Panel::Prompt,
            Self::Bubble => Panel::Bubble,
        }
    }
}

/// A rectangle in logical points, origin top-left (Tauri's convention,
/// not `AppKit`'s).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn mid_x(&self) -> f64 {
        self.x + self.width / 2.0
    }

    pub fn max_x(&self) -> f64 {
        self.x + self.width
    }

    pub fn max_y(&self) -> f64 {
        self.y + self.height
    }

    pub fn contains_point(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.max_x() && y >= self.y && y < self.max_y()
    }

    /// Whether `other` lies wholly inside, edges included.
    pub fn contains(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.max_x() <= self.max_x()
            && other.max_y() <= self.max_y()
    }
}

/// Where the panel hangs: the top-centre point of its frame and the
/// visible frame of the screen it was saved on.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PanelAnchor {
    pub top_center: (f64, f64),
    pub screen: Rect,
}

impl PanelAnchor {
    /// The default on a screen's visible frame: top centre, `DEFAULT_TOP_INSET`
    /// under the top edge.
    pub fn default_in(screen: Rect) -> Self {
        Self {
            top_center: (screen.mid_x(), screen.y + DEFAULT_TOP_INSET),
            screen,
        }
    }

    /// The frame of a panel of `size` hanging from the anchor, on whole
    /// points.
    pub fn frame_for(&self, size: (f64, f64)) -> Rect {
        Rect::new(
            (self.top_center.0 - size.0 / 2.0).round(),
            self.top_center.1.round(),
            size.0,
            size.1,
        )
    }

    /// The anchor that describes a panel at `frame`, on the screen among
    /// `screens` that holds its top-centre point (else `fallback`).
    pub fn from_frame(frame: Rect, screens: &[Rect], fallback: Rect) -> Self {
        let point = (frame.mid_x(), frame.y);
        let screen = screens
            .iter()
            .copied()
            .find(|screen| screen.contains_point(point.0, point.1))
            .unwrap_or(fallback);
        Self {
            top_center: point,
            screen,
        }
    }

    /// A saved anchor is kept while a panel of `size` hanging from it lies
    /// within one of the current screens; otherwise the default on
    /// `fallback`.
    pub fn validated(
        saved: Option<Self>,
        size: (f64, f64),
        screens: &[Rect],
        fallback: Rect,
    ) -> Self {
        if let Some(saved) = saved
            && screens
                .iter()
                .any(|screen| screen.contains(&saved.frame_for(size)))
        {
            return saved;
        }
        Self::default_in(fallback)
    }
}

/// Two sizes within a point of each other are the same size (the window
/// system rounds to the pixel grid).
fn matches(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1.0 && (a.1 - b.1).abs() < 1.0
}

/// The panels' state, managed by the app.
#[derive(Debug, Default)]
pub struct Panels {
    anchor: Mutex<Option<PanelAnchor>>,
    anchor_loaded: Mutex<bool>,
    /// The size each panel's page last reported.
    sizes: Mutex<HashMap<Panel, (f64, f64)>>,
    prompt: Mutex<Option<PromptRequest>>,
    recording: Mutex<RecordingState>,
}

impl Panels {
    fn size_of(&self, panel: Panel) -> (f64, f64) {
        self.sizes
            .lock()
            .ok()
            .and_then(|sizes| sizes.get(&panel).copied())
            .unwrap_or(panel.initial_size())
    }

    pub fn content(&self) -> Option<FloatingContent> {
        let prompt = self.prompt.lock().ok().and_then(|prompt| prompt.clone());
        let recording = self
            .recording
            .lock()
            .map(|state| *state)
            .unwrap_or_default();
        FloatingContent::resolve(prompt.as_ref(), recording)
    }
}

const ANCHOR_FILE: &str = "panel-anchor.json";

fn anchor_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join(ANCHOR_FILE))
}

/// The screens' work areas in logical points, the primary first.
fn screens(app: &AppHandle) -> Vec<Rect> {
    let primary = app.primary_monitor().ok().flatten();
    let mut all = app.available_monitors().unwrap_or_default();
    if let Some(primary) = primary {
        all.retain(|monitor| monitor.position() != primary.position());
        all.insert(0, primary);
    }
    all.iter()
        .map(|monitor| {
            let scale = monitor.scale_factor();
            let area = monitor.work_area();
            Rect::new(
                f64::from(area.position.x) / scale,
                f64::from(area.position.y) / scale,
                f64::from(area.size.width) / scale,
                f64::from(area.size.height) / scale,
            )
        })
        .collect()
}

fn fallback_screen(screens: &[Rect]) -> Rect {
    screens
        .first()
        .copied()
        .unwrap_or(Rect::new(0.0, 0.0, 1280.0, 800.0))
}

/// The anchor to lay out from, loading the saved one on first use.
fn current_anchor(app: &AppHandle, size: (f64, f64)) -> PanelAnchor {
    let panels = app.state::<Panels>();
    let mut anchor = panels.anchor.lock().expect("anchor");
    if let Ok(mut loaded) = panels.anchor_loaded.lock()
        && !*loaded
    {
        *loaded = true;
        *anchor = anchor_path(app)
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    }
    let screens = screens(app);
    let resolved = PanelAnchor::validated(*anchor, size, &screens, fallback_screen(&screens));
    *anchor = Some(resolved);
    resolved
}

fn save_anchor(app: &AppHandle, anchor: PanelAnchor) {
    let Some(path) = anchor_path(app) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec(&anchor) {
        let _ = fs::write(path, json);
    }
}

/// Shows `panel` at the anchor, creating its window when needed. The
/// prompt is recreated so its route carries the new request; the bubble
/// is reused.
pub fn show(app: &AppHandle, panel: Panel, query: Option<&str>) -> tauri::Result<WebviewWindow> {
    let size = app.state::<Panels>().size_of(panel);
    let frame = current_anchor(app, size).frame_for(size);
    show_at(app, panel, query, (frame.x, frame.y))
}

/// `show` at an explicit position (the smoke run lays the panels out
/// beside the windows).
pub fn show_at(
    app: &AppHandle,
    panel: Panel,
    query: Option<&str>,
    position: (f64, f64),
) -> tauri::Result<WebviewWindow> {
    if let Some(existing) = app.get_webview_window(panel.label()) {
        if panel == Panel::Prompt {
            existing.close()?;
        } else {
            existing.set_position(LogicalPosition::new(position.0, position.1))?;
            existing.show()?;
            return Ok(existing);
        }
    }
    let size = app.state::<Panels>().size_of(panel);
    let dev_server: Option<Url> = if cfg!(dev) {
        app.config().build.dev_url.clone()
    } else {
        None
    };
    let window = WebviewWindowBuilder::new(
        app,
        panel.label(),
        WebviewUrl::App(panel.start_path(query).into()),
    )
    .title("Steno")
    .inner_size(size.0, size.1)
    .position(position.0, position.1)
    .decorations(false)
    .transparent(true)
    .shadow(false)
    .always_on_top(true)
    .visible_on_all_workspaces(true)
    .skip_taskbar(true)
    .resizable(false)
    .focused(false)
    .accept_first_mouse(true)
    .on_navigation(move |url| navigation::allows(url, dev_server.as_ref()))
    .on_new_window(|_url, _features| NewWindowResponse::Deny)
    .build()?;
    #[cfg(target_os = "macos")]
    {
        let converted = window.clone();
        app.run_on_main_thread(move || {
            if let Err(error) = macos::make_panel(&converted) {
                eprintln!("[steno-desktop] panel {}: {error}", converted.label());
            }
        })?;
    }
    Ok(window)
}

/// Hides `panel` when it exists; the window stays for the next show.
pub fn hide(app: &AppHandle, panel: Panel) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(panel.label()) {
        window.hide()?;
    }
    Ok(())
}

/// Shows what `content` says and hides the other panel.
pub fn apply(app: &AppHandle, content: Option<&FloatingContent>) {
    let shown = content.map(FloatingContent::panel);
    for panel in Panel::ALL {
        if Some(panel) != shown
            && let Err(error) = hide(app, panel)
        {
            eprintln!(
                "[steno-desktop] hiding the {} panel failed: {error}",
                panel.label()
            );
        }
    }
    let Some(content) = content else {
        return;
    };
    let query = match content {
        FloatingContent::Prompt(request) => Some(request.query()),
        FloatingContent::Bubble => None,
    };
    if let Err(error) = show(app, content.panel(), query.as_deref()) {
        eprintln!(
            "[steno-desktop] showing the {} panel failed: {error}",
            content.panel().label()
        );
    }
}

fn refresh(app: &AppHandle) {
    let content = app.state::<Panels>().content();
    apply(app, content.as_ref());
}

/// A `recording` snapshot reached the main window.
pub fn note_recording(app: &AppHandle, state: RecordingState) {
    if let Ok(mut recording) = app.state::<Panels>().recording.lock() {
        if *recording == state {
            return;
        }
        *recording = state;
    }
    refresh(app);
}

/// The host raised (`Some`) or cleared (`None`) the detection prompt.
pub fn set_prompt(app: &AppHandle, request: Option<PromptRequest>) {
    if let Ok(mut prompt) = app.state::<Panels>().prompt.lock() {
        *prompt = request;
    }
    refresh(app);
}

/// The prompt's X: the prompt goes away. The host's detection controller
/// learns of it through `WP6b`'s hook here; the fixture host has no
/// detection to tell.
pub fn dismiss_prompt(app: &AppHandle) {
    set_prompt(app, None);
}

/// The page measured its content: the window takes that size around the
/// top-centre point it already hangs from (the anchor in the normal flow,
/// wherever the smoke put it otherwise), so a change of content never
/// moves the panel.
pub fn resize(app: &AppHandle, panel: Panel, size: (f64, f64)) -> tauri::Result<()> {
    if size.0 <= 0.0 || size.1 <= 0.0 {
        return Ok(());
    }
    let panels = app.state::<Panels>();
    if let Ok(mut sizes) = panels.sizes.lock() {
        if sizes.get(&panel).is_some_and(|last| matches(*last, size)) {
            return Ok(());
        }
        sizes.insert(panel, size);
    }
    let Some(window) = app.get_webview_window(panel.label()) else {
        return Ok(());
    };
    let scale = window.scale_factor()?;
    let position = window.outer_position()?;
    let current = window.inner_size()?;
    let top_center = (
        f64::from(position.x) / scale + f64::from(current.width) / scale / 2.0,
        f64::from(position.y) / scale,
    );
    let frame = resized_frame(top_center, size);
    window.set_size(LogicalSize::new(size.0, size.1))?;
    window.set_position(LogicalPosition::new(frame.x, frame.y))?;
    Ok(())
}

/// The frame of a panel of `size` whose top-centre point stays at
/// `top_center`.
pub fn resized_frame(top_center: (f64, f64), size: (f64, f64)) -> Rect {
    PanelAnchor {
        top_center,
        screen: Rect::new(0.0, 0.0, 0.0, 0.0),
    }
    .frame_for(size)
}

/// The window moved. A move with the size the page last reported is a
/// drag and updates the anchor; one with another size is the window still
/// taking its content's size, and is not.
pub fn moved(app: &AppHandle, panel: Panel, position: PhysicalPosition<i32>) {
    let Some(window) = app.get_webview_window(panel.label()) else {
        return;
    };
    let (Ok(scale), Ok(inner)) = (window.scale_factor(), window.inner_size()) else {
        return;
    };
    let size = (
        f64::from(inner.width) / scale,
        f64::from(inner.height) / scale,
    );
    let panels = app.state::<Panels>();
    let reported = panels.size_of(panel);
    if !matches(size, reported) {
        return;
    }
    let frame = Rect::new(
        f64::from(position.x) / scale,
        f64::from(position.y) / scale,
        size.0,
        size.1,
    );
    let screens = screens(app);
    let anchor = PanelAnchor::from_frame(frame, &screens, fallback_screen(&screens));
    let changed = panels.anchor.lock().is_ok_and(|mut current| {
        if *current == Some(anchor) {
            false
        } else {
            *current = Some(anchor);
            true
        }
    });
    if changed {
        save_anchor(app, anchor);
    }
}

#[cfg(target_os = "macos")]
mod macos {
    //! The Swift `FloatingPanel`: a borderless non-activating `NSPanel` at
    //! the floating level that joins every space, shows beside full-screen
    //! apps, never becomes key or main, keeps showing when the app
    //! deactivates, and moves by its background.

    use tauri::WebviewWindow;
    use tauri_nspanel::{CollectionBehavior, Panel as _, PanelLevel, StyleMask, tauri_panel};

    tauri_panel! {
        panel!(FloatingPanel {
            config: {
                can_become_key_window: false,
                can_become_main_window: false,
                is_floating_panel: true,
                becomes_key_only_if_needed: true,
                hides_on_deactivate: false
            }
        })
    }

    pub fn make_panel(window: &WebviewWindow) -> tauri::Result<()> {
        let panel = FloatingPanel::from_window(window)?;
        panel.set_level(PanelLevel::Floating.value());
        if let Err(error) = panel.set_style_mask(
            StyleMask::empty()
                .nonactivating_panel()
                .borderless()
                .value(),
        ) {
            eprintln!("[steno-desktop] panel style mask: {error}");
        }
        panel.set_collection_behavior(
            CollectionBehavior::new()
                .can_join_all_spaces()
                .full_screen_auxiliary()
                .stationary()
                .value(),
        );
        panel.set_hides_on_deactivate(false);
        panel.set_movable_by_window_background(true);
        panel.set_has_shadow(true);
        panel.set_opaque(false);
        panel.set_released_when_closed(false);
        panel.show();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect::new(0.0, 25.0, 1440.0, 875.0);
    const SECOND: Rect = Rect::new(1440.0, 0.0, 1920.0, 1080.0);

    #[test]
    fn labels_and_routes_are_the_web_apps() {
        for panel in Panel::ALL {
            assert_eq!(Panel::from_label(panel.label()), Some(panel));
            assert!(panel.route().ends_with(panel.label()));
        }
        assert_eq!(Panel::from_label("main"), None);
        assert_eq!(Panel::Bubble.start_path(None), "index.html#/panel/bubble");
        assert_eq!(
            Panel::Prompt.start_path(Some("app=Zoom&seconds=60")),
            "index.html#/panel/prompt?app=Zoom&seconds=60"
        );
        assert_eq!(
            Panel::Prompt.start_path(Some("")),
            "index.html#/panel/prompt"
        );
    }

    #[test]
    fn the_prompt_query_is_form_encoded() {
        let request = PromptRequest {
            app_name: "Microsoft Teams (work or school)".into(),
            seconds: 60,
        };
        assert_eq!(
            request.query(),
            "app=Microsoft+Teams+%28work+or+school%29&seconds=60"
        );
    }

    #[test]
    fn a_busy_recorder_wins_then_the_prompt_then_nothing() {
        let prompt = PromptRequest {
            app_name: "Zoom".into(),
            seconds: 60,
        };
        for state in [
            RecordingState::Starting,
            RecordingState::Recording,
            RecordingState::Stopping,
        ] {
            assert_eq!(
                FloatingContent::resolve(Some(&prompt), state),
                Some(FloatingContent::Bubble),
                "{state:?}"
            );
            assert_eq!(
                FloatingContent::resolve(None, state),
                Some(FloatingContent::Bubble)
            );
        }
        assert_eq!(
            FloatingContent::resolve(Some(&prompt), RecordingState::Idle),
            Some(FloatingContent::Prompt(prompt.clone()))
        );
        assert_eq!(FloatingContent::resolve(None, RecordingState::Idle), None);
        assert_eq!(FloatingContent::Prompt(prompt).panel(), Panel::Prompt);
        assert_eq!(FloatingContent::Bubble.panel(), Panel::Bubble);
    }

    #[test]
    fn the_default_anchor_is_top_centre_under_the_top_edge() {
        let anchor = PanelAnchor::default_in(SCREEN);
        assert_eq!(anchor.top_center, (720.0, 33.0));
        assert_eq!(anchor.screen, SCREEN);
        let frame = anchor.frame_for((480.0, 56.0));
        assert_eq!(frame, Rect::new(480.0, 33.0, 480.0, 56.0));
        // Both contents hang from the same point.
        let bubble = anchor.frame_for((240.0, 40.0));
        assert_eq!(bubble.mid_x(), frame.mid_x());
        assert_eq!(bubble.y, frame.y);
    }

    #[test]
    fn a_resize_keeps_the_top_centre() {
        let before = Rect::new(1160.0, 700.0, 480.0, 56.0);
        let after = resized_frame((before.mid_x(), before.y), (384.0, 56.0));
        assert_eq!(after, Rect::new(1208.0, 700.0, 384.0, 56.0));
        assert_eq!(after.mid_x(), before.mid_x());
        let taller = resized_frame((after.mid_x(), after.y), (384.0, 68.0));
        assert_eq!((taller.x, taller.y), (after.x, after.y));
    }

    #[test]
    fn frames_land_on_whole_points() {
        let anchor = PanelAnchor {
            top_center: (100.3, 20.6),
            screen: SCREEN,
        };
        let frame = anchor.frame_for((33.0, 40.0));
        assert_eq!((frame.x, frame.y), (84.0, 21.0));
    }

    #[test]
    fn a_dragged_frame_becomes_the_anchor_on_its_screen() {
        let frame = Rect::new(1500.0, 100.0, 240.0, 40.0);
        let anchor = PanelAnchor::from_frame(frame, &[SCREEN, SECOND], SCREEN);
        assert_eq!(anchor.top_center, (1620.0, 100.0));
        assert_eq!(anchor.screen, SECOND);
        // Off every screen: the fallback is recorded as the screen.
        let off =
            PanelAnchor::from_frame(Rect::new(-500.0, -500.0, 240.0, 40.0), &[SCREEN], SCREEN);
        assert_eq!(off.screen, SCREEN);
    }

    #[test]
    fn a_saved_anchor_survives_while_its_panel_fits_a_current_screen() {
        let saved = PanelAnchor {
            top_center: (1620.0, 100.0),
            screen: SECOND,
        };
        assert_eq!(
            PanelAnchor::validated(Some(saved), (240.0, 40.0), &[SCREEN, SECOND], SCREEN),
            saved
        );
        // The second screen is gone: back to the default on the main one.
        assert_eq!(
            PanelAnchor::validated(Some(saved), (240.0, 40.0), &[SCREEN], SCREEN),
            PanelAnchor::default_in(SCREEN)
        );
        // A panel whose bottom would hang below the screen does not fit,
        // even though its anchor point lies inside.
        let low = PanelAnchor {
            top_center: (720.0, 880.0),
            screen: SCREEN,
        };
        assert_eq!(
            PanelAnchor::validated(Some(low), (240.0, 40.0), &[SCREEN], SCREEN),
            PanelAnchor::default_in(SCREEN)
        );
        assert_eq!(
            PanelAnchor::validated(None, (240.0, 40.0), &[SCREEN], SCREEN),
            PanelAnchor::default_in(SCREEN)
        );
    }

    #[test]
    fn the_anchor_round_trips_through_json() {
        let anchor = PanelAnchor {
            top_center: (720.0, 33.0),
            screen: SCREEN,
        };
        let json = serde_json::to_string(&anchor).unwrap();
        assert_eq!(serde_json::from_str::<PanelAnchor>(&json).unwrap(), anchor);
    }

    #[test]
    fn sub_point_differences_are_the_same_size() {
        assert!(matches((240.0, 40.0), (240.4, 39.6)));
        assert!(!matches((240.0, 40.0), (241.0, 40.0)));
        assert!(!matches((240.0, 40.0), (240.0, 68.0)));
    }

    #[test]
    fn rects_contain_points_and_rects() {
        assert!(SCREEN.contains_point(0.0, 25.0));
        assert!(!SCREEN.contains_point(1440.0, 25.0));
        assert!(SCREEN.contains(&Rect::new(0.0, 25.0, 1440.0, 875.0)));
        assert!(!SCREEN.contains(&Rect::new(0.0, 24.0, 10.0, 10.0)));
        assert_eq!(SCREEN.max_y(), 900.0);
    }

    #[test]
    fn the_panels_state_resolves_from_what_it_holds() {
        let panels = Panels::default();
        assert_eq!(panels.content(), None);
        assert_eq!(panels.size_of(Panel::Bubble), (240.0, 40.0));
        *panels.prompt.lock().unwrap() = Some(PromptRequest {
            app_name: "Zoom".into(),
            seconds: 60,
        });
        assert!(matches!(panels.content(), Some(FloatingContent::Prompt(_))));
        *panels.recording.lock().unwrap() = RecordingState::Recording;
        assert_eq!(panels.content(), Some(FloatingContent::Bubble));
        panels
            .sizes
            .lock()
            .unwrap()
            .insert(Panel::Bubble, (300.0, 68.0));
        assert_eq!(panels.size_of(Panel::Bubble), (300.0, 68.0));
    }
}
