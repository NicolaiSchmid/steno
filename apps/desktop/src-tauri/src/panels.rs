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
//! Each panel's window is created once and then hidden and shown; the
//! prompt's is navigated to the new request each time one is raised, so
//! the page remounts and its countdown starts afresh.
//!
//! Both panels hang from one anchor, the top-centre point of the frame, so
//! the prompt turns into the bubble without moving; the user drags a
//! panel by its background (`data-tauri-drag-region` in the page), the
//! anchor follows and is saved. A saved anchor on a screen that is gone
//! falls back to the default: top centre of the main screen, 8 pt under
//! its top edge. The page measures itself and reports its size through
//! `panel_call("resize")`; the shell sizes the window from that, as the
//! Swift root reported through `contentSizeDidChange`. The geometry is
//! `panel_geometry.rs`.
//!
//! Swift: `FloatingPanel.swift`, `FloatingPanelModel.swift`,
//! `FloatingContent.swift`.

use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, PhysicalPosition, Url, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder, webview::NewWindowResponse,
};

use crate::{
    bridge::{BridgeError, failed},
    navigation,
    panel_geometry::{PROBE_SIZE, PanelAnchor, Rect, accepted_size, frame_hanging_from, same_size},
    recording::{RecorderState, RecordingState},
    windows,
};

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

    /// The hash route without its `#`.
    pub const fn route(self) -> &'static str {
        match self {
            Self::Bubble => "/panel/bubble",
            Self::Prompt => "/panel/prompt",
        }
    }

    /// Whether a new request reaches an existing window by navigating it:
    /// the prompt carries its request in the route; the bubble reads the
    /// recorder off the bridge and is only shown.
    pub const fn navigates_per_request(self) -> bool {
        matches!(self, Self::Prompt)
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

    /// The document fragment: `/panel/<label>[?<query>]`.
    pub fn fragment(self, query: Option<&str>) -> String {
        windows::with_query(self.route(), query)
    }

    /// `index.html#/panel/<label>?<query>`, for a new window.
    pub fn start_path(self, query: Option<&str>) -> String {
        format!("index.html#{}", self.fragment(query))
    }

    /// The URL an existing window loads for a new request: the document it
    /// already shows with the new fragment, so the origin (the app's or the
    /// dev server's) is whatever the window has.
    pub fn route_url(self, current: &Url, query: Option<&str>) -> Url {
        let mut url = current.clone();
        url.set_fragment(Some(&self.fragment(query)));
        url
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
    /// The prompt's query: `app=<name>&seconds=<n>`, form-encoded, plus
    /// `raised=<serial>` when the shell numbers the request: a new number
    /// is a new document for the page, so an identical request raised
    /// again still remounts the prompt and restarts its countdown.
    pub fn query(&self, raised: Option<u64>) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query
            .append_pair("app", &self.app_name)
            .append_pair("seconds", &self.seconds.to_string());
        if let Some(raised) = raised {
            query.append_pair("raised", &raised.to_string());
        }
        query.finish()
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

/// The panels' state, managed by the app.
#[derive(Debug)]
pub struct Panels {
    anchor: Mutex<Option<PanelAnchor>>,
    anchor_loaded: Mutex<bool>,
    /// The size each panel's page last reported.
    sizes: Mutex<HashMap<Panel, (f64, f64)>>,
    prompt: Mutex<Option<PromptRequest>>,
    recording: Mutex<RecordingState>,
    /// What `apply` last showed, so a snapshot that changes nothing does
    /// not re-navigate the prompt.
    showing: Mutex<Option<FloatingContent>>,
    /// How many prompts have been raised; the next one's `raised` number.
    raised: AtomicU64,
}

impl Default for Panels {
    fn default() -> Self {
        Self {
            anchor: Mutex::default(),
            anchor_loaded: Mutex::default(),
            sizes: Mutex::default(),
            prompt: Mutex::default(),
            recording: Mutex::new(RecordingState::Idle),
            showing: Mutex::default(),
            raised: AtomicU64::new(0),
        }
    }
}

impl Panels {
    /// The size the page last reported, if it has.
    fn measured(&self, panel: Panel) -> Option<(f64, f64)> {
        self.sizes
            .lock()
            .ok()
            .and_then(|sizes| sizes.get(&panel).copied())
    }

    /// The measured size, else the one to lay out with before measuring.
    fn size_of(&self, panel: Panel) -> (f64, f64) {
        self.measured(panel).unwrap_or(panel.initial_size())
    }

    pub fn content(&self) -> Option<FloatingContent> {
        let prompt = self.prompt.lock().ok().and_then(|prompt| prompt.clone());
        let recording = self
            .recording
            .lock()
            .map_or(RecordingState::Idle, |state| *state);
        FloatingContent::resolve(prompt.as_ref(), recording)
    }

    /// Records what is about to show; `false` when it already is.
    fn note_showing(&self, content: Option<&FloatingContent>) -> bool {
        self.showing.lock().is_ok_and(|mut showing| {
            if showing.as_ref() == content {
                false
            } else {
                *showing = content.cloned();
                true
            }
        })
    }

    /// The query for a prompt raised now, numbered.
    fn prompt_query(&self, request: &PromptRequest) -> String {
        request.query(Some(self.raised.fetch_add(1, Ordering::SeqCst) + 1))
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

/// The anchor to lay out from, loading the saved one on first use and
/// validating it for a panel of `size`. The screens are asked for before
/// the lock is taken: off the main thread the query waits on it, and the
/// main thread takes the same lock in `moved`.
fn current_anchor(app: &AppHandle, size: (f64, f64)) -> PanelAnchor {
    let screens = screens(app);
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

/// The window's inner size in logical points.
pub fn logical_size(window: &WebviewWindow) -> tauri::Result<(f64, f64)> {
    let scale = window.scale_factor()?;
    let size = window.inner_size()?;
    Ok((
        f64::from(size.width) / scale,
        f64::from(size.height) / scale,
    ))
}

/// The window's top-centre point in logical points.
fn top_center_of(window: &WebviewWindow) -> tauri::Result<(f64, f64)> {
    let scale = window.scale_factor()?;
    let position = window.outer_position()?;
    let (width, _) = logical_size(window)?;
    Ok((
        f64::from(position.x) / scale + width / 2.0,
        f64::from(position.y) / scale,
    ))
}

/// Shows `panel` at the anchor, creating its window when needed. The
/// anchor is validated with the size the page reported, or with
/// `PROBE_SIZE` before it has, never with the pre-measure maximum.
pub fn show(app: &AppHandle, panel: Panel, query: Option<&str>) -> tauri::Result<WebviewWindow> {
    let panels = app.state::<Panels>();
    let size = panels.size_of(panel);
    let probe = panels.measured(panel).unwrap_or(PROBE_SIZE);
    let frame = current_anchor(app, probe).frame_for(size);
    show_at(app, panel, query, (frame.x, frame.y))
}

/// `show` at an explicit position (the smoke run lays the panels out
/// beside the windows). An existing window is reused: the prompt's is
/// navigated to the new request first.
pub fn show_at(
    app: &AppHandle,
    panel: Panel,
    query: Option<&str>,
    position: (f64, f64),
) -> tauri::Result<WebviewWindow> {
    if let Some(existing) = app.get_webview_window(panel.label()) {
        if panel.navigates_per_request() {
            existing.navigate(panel.route_url(&existing.url()?, query))?;
        }
        existing.set_position(LogicalPosition::new(position.0, position.1))?;
        existing.show()?;
        return Ok(existing);
    }
    let size = app.state::<Panels>().size_of(panel);
    let dev_server = navigation::dev_server(app);
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
    // GTK ignores `resize` on a window it holds non-resizable and keeps
    // the webview's natural size instead, so the panel would never take
    // the size its page reports there; undecorated, the window has no
    // edge for the user to resize by either way. macOS and Windows honour
    // `set_size` on a fixed window.
    .resizable(cfg!(target_os = "linux"))
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

/// Shows what `content` says and hides the other panel; nothing when it
/// is what already shows. Runs on the main thread (`refresh`).
fn apply(app: &AppHandle, content: Option<&FloatingContent>) {
    let panels = app.state::<Panels>();
    if !panels.note_showing(content) {
        return;
    }
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
        FloatingContent::Prompt(request) => Some(panels.prompt_query(request)),
        FloatingContent::Bubble => None,
    };
    if let Err(error) = show(app, content.panel(), query.as_deref()) {
        eprintln!(
            "[steno-desktop] showing the {} panel failed: {error}",
            content.panel().label()
        );
    }
}

/// Re-applies the one rule, on the main thread: every `apply` runs there
/// one after another, so two threads can never interleave a show and a
/// hide, and the window calls inside it (monitors, URLs, a new window)
/// run at once instead of waiting on the main thread from a worker.
fn refresh(app: &AppHandle) {
    let handle = app.clone();
    let queued = app.run_on_main_thread(move || {
        let content = handle.state::<Panels>().content();
        apply(&handle, content.as_ref());
    });
    if let Err(error) = queued {
        eprintln!("[steno-desktop] updating the panels failed: {error}");
    }
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
/// moves the panel. A report that is not a size is `invalidParams`; one
/// larger than the screen is clamped to its work area.
pub fn resize(app: &AppHandle, panel: Panel, reported: (f64, f64)) -> Result<(), BridgeError> {
    let screens = screens(app);
    let fallback = fallback_screen(&screens);
    let window = app.get_webview_window(panel.label());
    let top_center = window
        .as_ref()
        .map(top_center_of)
        .transpose()
        .map_err(failed)?;
    let screen = top_center.map_or(fallback, |point| Rect::holding(&screens, point, fallback));
    let size = accepted_size(reported, (screen.width, screen.height)).ok_or_else(|| {
        BridgeError::invalid_params(format!(
            "resize: {} by {} is not a size",
            reported.0, reported.1
        ))
    })?;
    let panels = app.state::<Panels>();
    if let Ok(mut sizes) = panels.sizes.lock() {
        if sizes.get(&panel).is_some_and(|last| same_size(*last, size)) {
            return Ok(());
        }
        sizes.insert(panel, size);
    }
    let (Some(window), Some(top_center)) = (window, top_center) else {
        return Ok(());
    };
    let frame = frame_hanging_from(top_center, size);
    window
        .set_size(LogicalSize::new(size.0, size.1))
        .map_err(failed)?;
    window
        .set_position(LogicalPosition::new(frame.x, frame.y))
        .map_err(failed)?;
    Ok(())
}

/// The window moved. A move with the size the page last reported is a
/// drag and updates the anchor; one with another size is the window still
/// taking its content's size, and is not.
pub fn moved(app: &AppHandle, panel: Panel, position: PhysicalPosition<i32>) {
    let Some(window) = app.get_webview_window(panel.label()) else {
        return;
    };
    let (Ok(scale), Ok(size)) = (window.scale_factor(), logical_size(&window)) else {
        return;
    };
    let panels = app.state::<Panels>();
    let reported = panels.size_of(panel);
    if !same_size(size, reported) {
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

    fn request(app_name: &str) -> PromptRequest {
        PromptRequest {
            app_name: app_name.into(),
            seconds: 60,
        }
    }

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
    fn the_prompt_query_is_form_encoded_and_numbered_when_raised() {
        let request = request("Microsoft Teams (work or school)");
        assert_eq!(
            request.query(None),
            "app=Microsoft+Teams+%28work+or+school%29&seconds=60"
        );
        assert_eq!(
            request.query(Some(3)),
            "app=Microsoft+Teams+%28work+or+school%29&seconds=60&raised=3"
        );
    }

    /// The second prompt reuses the window: the prompt's window is
    /// navigated, on whichever origin it already has, and each raised
    /// request gets a new number so the page remounts even for the same
    /// app; the bubble's window is only shown.
    #[test]
    fn a_second_prompt_reuses_the_window_by_navigating_it() {
        assert!(Panel::Prompt.navigates_per_request());
        assert!(!Panel::Bubble.navigates_per_request());
        let panels = Panels::default();
        let first = panels.prompt_query(&request("Zoom"));
        let second = panels.prompt_query(&request("Zoom"));
        assert_eq!(first, "app=Zoom&seconds=60&raised=1");
        assert_eq!(second, "app=Zoom&seconds=60&raised=2");
        for origin in [
            "tauri://localhost/index.html#/panel/prompt?app=Zoom&seconds=60&raised=1",
            "http://tauri.localhost/index.html#/panel/prompt",
            "http://localhost:5173/index.html",
        ] {
            let current = Url::parse(origin).unwrap();
            let next = Panel::Prompt.route_url(&current, Some(&second));
            assert_eq!(next.scheme(), current.scheme());
            assert_eq!(next.host_str(), current.host_str());
            assert_eq!(next.port(), current.port());
            assert_eq!(next.path(), "/index.html");
            assert_eq!(
                next.fragment(),
                Some("/panel/prompt?app=Zoom&seconds=60&raised=2"),
                "{origin}"
            );
        }
    }

    #[test]
    fn a_busy_recorder_wins_then_the_prompt_then_nothing() {
        let prompt = request("Zoom");
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
    fn the_panels_state_resolves_from_what_it_holds() {
        let panels = Panels::default();
        assert_eq!(panels.content(), None);
        assert_eq!(panels.measured(Panel::Bubble), None);
        assert_eq!(panels.size_of(Panel::Bubble), Panel::Bubble.initial_size());
        assert_eq!(Panel::Bubble.initial_size(), (480.0, 40.0));
        assert_eq!(Panel::Prompt.initial_size(), (480.0, 56.0));
        *panels.prompt.lock().unwrap() = Some(request("Zoom"));
        assert!(matches!(panels.content(), Some(FloatingContent::Prompt(_))));
        *panels.recording.lock().unwrap() = RecordingState::Recording;
        assert_eq!(panels.content(), Some(FloatingContent::Bubble));
        panels
            .sizes
            .lock()
            .unwrap()
            .insert(Panel::Bubble, (300.0, 68.0));
        assert_eq!(panels.measured(Panel::Bubble), Some((300.0, 68.0)));
        assert_eq!(panels.size_of(Panel::Bubble), (300.0, 68.0));
    }

    /// A snapshot that changes nothing leaves the panel alone; a prompt
    /// shown after a dismissal is a change, as is the same request after
    /// the bubble.
    #[test]
    fn apply_moves_only_on_a_change_of_content() {
        let panels = Panels::default();
        let prompt = FloatingContent::Prompt(request("Zoom"));
        assert!(!panels.note_showing(None));
        assert!(panels.note_showing(Some(&prompt)));
        assert!(!panels.note_showing(Some(&prompt)));
        assert!(panels.note_showing(None));
        assert!(panels.note_showing(Some(&prompt)));
        assert!(panels.note_showing(Some(&FloatingContent::Bubble)));
        assert!(panels.note_showing(Some(&prompt)));
    }
}
