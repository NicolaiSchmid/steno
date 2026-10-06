//! The three windows: the Swift app's routes, sizes and minimums. Main opens
//! at start and is hidden, not destroyed, when the user closes it
//! (`main.rs`), so the tray and the panels always have it; Settings and
//! onboarding are created on `window.open` and focused when they already
//! exist. A meeting or a section asked of a window whose page has not
//! mounted yet waits in `Pages` and is published on its `page.ready`.
//!
//! On Linux a closed Settings or onboarding window is kept, not destroyed
//! (`Retired`): `WebKitGTK` leaks a file descriptor for every webview
//! destroyed (#160), so a long session that opens Settings often would run
//! out of them. The kept window drops its page (`about:blank`) and hides;
//! opened again, it loads its route as a new window would, with the query
//! asked for, so the user sees a fresh window. macOS and Windows destroy
//! the window as before.
//!
//! Swift: the sizes live in `StenoApp.swift` (main), `SettingsWindow.swift`
//! and `OnboardingWindow.swift`, main's minimum in `MainWindow.swift`.

use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

pub use steno_bridge::BridgeWindow;
use steno_bridge::WindowParams;
use steno_core::json::uuid_string;
use tauri::{
    AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    webview::NewWindowResponse,
};

use crate::{
    bridge::{BridgeError, failed},
    host::Host,
    navigation,
};

/// One window as the Swift app sizes it, in logical points. Its label is
/// the contract's `BridgeWindow::as_str`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spec {
    pub title: &'static str,
    pub route: &'static str,
    pub size: (f64, f64),
    /// `None` for a window of fixed size.
    pub min_size: Option<(f64, f64)>,
}

impl Spec {
    /// The shell's own knowledge of a window, kept off the contract's
    /// `BridgeWindow`.
    pub const fn of(window: BridgeWindow) -> Spec {
        match window {
            BridgeWindow::Main => Spec {
                title: "Steno",
                route: "#/main",
                size: (1120.0, 720.0),
                min_size: Some((960.0, 600.0)),
            },
            BridgeWindow::Settings => Spec {
                title: "Settings",
                route: "#/settings",
                size: (960.0, 640.0),
                min_size: Some((760.0, 520.0)),
            },
            BridgeWindow::Onboarding => Spec {
                title: "Welcome to Steno",
                route: "#/onboarding",
                size: (560.0, 620.0),
                min_size: None,
            },
        }
    }
}

/// The document a window loads: `index.html` plus the hash route and its
/// query, resolved against the app origin by Tauri.
pub fn start_path(window: BridgeWindow, query: Option<&str>) -> String {
    format!("index.html{}", with_query(Spec::of(window).route, query))
}

/// `route?query`, or the route alone for no query or an empty one.
pub fn with_query(route: &str, query: Option<&str>) -> String {
    match query {
        Some(query) if !query.is_empty() => format!("{route}?{query}"),
        _ => route.to_owned(),
    }
}

/// Shows and focuses the window when it exists, creates it otherwise.
/// `position` places a new window (the smoke run lays the three out side
/// by side); the system places it when `None`.
pub fn open(
    app: &AppHandle,
    window: BridgeWindow,
    query: Option<&str>,
    position: Option<(f64, f64)>,
) -> tauri::Result<WebviewWindow> {
    let spec = Spec::of(window);
    if let Some(existing) = app.get_webview_window(window.as_str()) {
        if let Some(document) = app.state::<Retired>().revive(window.as_str()) {
            existing.navigate(reopened(&document, window, query))?;
        }
        existing.show()?;
        existing.set_focus()?;
        return Ok(existing);
    }

    let dev_server = navigation::dev_server(app);
    let mut builder = WebviewWindowBuilder::new(
        app,
        window.as_str(),
        WebviewUrl::App(start_path(window, query).into()),
    )
    .title(spec.title)
    .inner_size(spec.size.0, spec.size.1)
    .resizable(spec.min_size.is_some())
    .on_navigation(move |url| navigation::allows(url, dev_server.as_ref()))
    // A `target="_blank"` link or `window.open` from the page opens nothing:
    // external links go through `system.openURL`. wry already opens no
    // window when no handler is set (WebKitGTK's `create` signal unhandled,
    // WKWebView's delegate answering nil); the explicit `Deny` pins that
    // against a default change. Needs a webview to exercise, so no test.
    .on_new_window(|_url, _features| NewWindowResponse::Deny);
    if let Some((width, height)) = spec.min_size {
        builder = builder.min_inner_size(width, height);
    }
    if let Some((x, y)) = position {
        builder = builder.position(x, y);
    }
    // The Swift windows hide their title bar and let the page paint up to the
    // top edge, leaving the traffic lights their inset; the same look here.
    // Linux and Windows keep their native title bar. The page's
    // `TITLE_BAR_INSET` (`apps/macos/web/src/lib/platform.tsx`) follows this
    // `cfg`.
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
    }
    builder.build()
}

/// `window.open` from a page, or a deep link: opens or focuses the window.
/// A meeting for main, or a section for a Settings window that already
/// exists, rides on the window's next `app` snapshot
/// (`Host::publish_request`), once its page listens (`Pages`); a new
/// Settings window reads the section from its route.
///
/// Swift: `WindowRequests.swift`.
pub fn open_requested(
    app: &AppHandle,
    host: &Host,
    params: &WindowParams,
) -> Result<(), BridgeError> {
    let pages = app.state::<Pages>();
    // A kept window opens as a new one, its section in the route.
    let existed = app.get_webview_window(params.window.as_str()).is_some()
        && !app.state::<Retired>().holds(params.window.as_str());
    match params.window {
        BridgeWindow::Onboarding => {
            open(app, BridgeWindow::Onboarding, None, None).map_err(failed)?;
        }
        BridgeWindow::Main => {
            let window = open(app, BridgeWindow::Main, None, None).map_err(failed)?;
            if let Some(meeting_id) = params.meeting_id {
                request(host, &pages, &window, Request::meeting(&meeting_id))?;
            }
        }
        BridgeWindow::Settings => {
            let section = params.section;
            let query = section.map(|section| format!("section={section}"));
            let window =
                open(app, BridgeWindow::Settings, query.as_deref(), None).map_err(failed)?;
            if let (true, Some(section)) = (existed, section) {
                request(host, &pages, &window, Request::section(section))?;
            }
        }
    }
    Ok(())
}

steno_core::string_enum! {
    /// The `app` snapshot's two request fields, as `steno_bridge`'s
    /// `AppSnapshot` names them (`requested_meeting_id`,
    /// `requested_settings_section`); a test pins the two together.
    pub enum RequestField {
        MeetingId = "requestedMeetingID",
        SettingsSection = "requestedSettingsSection",
    }
}

/// A meeting or a section for a window, as the `app` snapshot carries it:
/// the request field and its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub field: RequestField,
    pub value: String,
}

impl Request {
    pub fn meeting(id: &::uuid::Uuid) -> Self {
        Self {
            field: RequestField::MeetingId,
            value: uuid_string(*id),
        }
    }

    pub fn section(section: steno_bridge::SettingsSection) -> Self {
        Self {
            field: RequestField::SettingsSection,
            value: section.as_str().to_owned(),
        }
    }
}

/// Which pages have mounted (`page.ready`) and what each window is still
/// owed. A request published before the page listens is lost (a deep link
/// at a cold launch reaches the main window before its page mounts; a
/// second instance's link can reach it the same way), so it waits here
/// and goes out right after the page's first snapshots. One lock over
/// both, so a `page.ready` on one thread and a request on another cannot
/// leave a request owed to a page that already listens. Managed state.
#[derive(Debug, Default)]
pub struct Pages {
    state: Mutex<PagesState>,
}

#[derive(Debug, Default)]
struct PagesState {
    ready: HashSet<String>,
    owed: HashMap<String, Request>,
}

impl Pages {
    /// The page of `label` sent `page.ready`: the request it was owed, if
    /// any, to publish now. A page that mounts again (a reload) owes
    /// nothing new.
    pub fn ready(&self, label: &str) -> Option<Request> {
        let mut state = self.state.lock().ok()?;
        state.ready.insert(label.to_owned());
        state.owed.remove(label)
    }

    #[cfg(test)]
    fn is_ready(&self, label: &str) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| state.ready.contains(label))
    }

    /// `request` for the page of `label`: handed back to publish now when
    /// the page listens, else kept for its `page.ready` (a later one
    /// replaces it) and `None`.
    pub fn publish_or_owe(&self, label: &str, request: Request) -> Option<Request> {
        let mut state = self.state.lock().ok()?;
        if state.ready.contains(label) {
            return Some(request);
        }
        state.owed.insert(label.to_owned(), request);
        None
    }

    /// The window of `label` was destroyed: its page is gone and so is
    /// anything it was owed.
    pub fn gone(&self, label: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.ready.remove(label);
            state.owed.remove(label);
        }
    }
}

/// Publishes `request` to `window` when its page listens, else owes it
/// to the page for its `page.ready`.
pub fn request(
    host: &Host,
    pages: &Pages,
    window: &WebviewWindow,
    request: Request,
) -> Result<(), BridgeError> {
    match pages.publish_or_owe(window.label(), request) {
        Some(now) => host.publish_request(window, now.field.as_str(), &now.value),
        None => Ok(()),
    }
}

/// Whether closing the window of `label` keeps it instead (`retire`):
/// Settings and onboarding on Linux, against `WebKitGTK`'s leak (#160). Main
/// has its own rule (`hides_on_close` in `main.rs`); the panels only ever
/// hide.
pub fn retires_on_close(label: &str) -> bool {
    cfg!(target_os = "linux")
        && (label == BridgeWindow::Settings.as_str() || label == BridgeWindow::Onboarding.as_str())
}

/// The windows kept after a close (`retires_on_close`), each with the
/// document it showed, for its next `open`. Managed state.
#[derive(Debug, Default)]
pub struct Retired(Mutex<HashMap<String, Url>>);

impl Retired {
    /// Notes `label` as kept, showing `document`; false when it already
    /// was, which keeps the document it showed then (a second close finds
    /// `about:blank`).
    fn note(&self, label: &str, document: Url) -> bool {
        let Ok(mut kept) = self.0.lock() else {
            return false;
        };
        match kept.entry(label.to_owned()) {
            std::collections::hash_map::Entry::Occupied(_) => false,
            std::collections::hash_map::Entry::Vacant(vacant) => {
                vacant.insert(document);
                true
            }
        }
    }

    /// Whether the window of `label` is kept.
    pub fn holds(&self, label: &str) -> bool {
        self.0.lock().is_ok_and(|kept| kept.contains_key(label))
    }

    /// The document the kept window of `label` showed, which it stops
    /// being kept for: it opens again.
    fn revive(&self, label: &str) -> Option<Url> {
        self.0.lock().ok()?.remove(label)
    }

    /// The window of `label` was destroyed; whether it was kept.
    pub fn forget(&self, label: &str) -> bool {
        self.revive(label).is_some()
    }
}

/// Keeps `window` after its close as a new one would start (`Retired`):
/// its page is gone (`Pages::gone`), it hides, and it shows `about:blank`,
/// so its next `open` loads a fresh document. False when it was kept
/// already (a second close), which changes nothing.
pub fn retire(app: &AppHandle, window: &WebviewWindow) -> tauri::Result<bool> {
    let label = window.label();
    if !app.state::<Retired>().note(label, window.url()?) {
        return Ok(false);
    }
    app.state::<Pages>().gone(label);
    window.hide()?;
    window.navigate(Url::parse("about:blank").expect("a URL"))?;
    Ok(true)
}

/// What a kept window loads when it opens again: the document it showed
/// (the app's origin or the dev server's) with `window`'s route and
/// `query`. Another document than the `about:blank` it shows, so the page
/// mounts afresh.
fn reopened(document: &Url, window: BridgeWindow, query: Option<&str>) -> Url {
    let mut url = document.clone();
    let route = Spec::of(window).route.trim_start_matches('#');
    url.set_fragment(Some(&with_query(route, query)));
    url
}

/// Closes the window when it exists; nothing otherwise.
pub fn close(app: &AppHandle, window: BridgeWindow) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(window.as_str()) {
        window.close()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_follow_the_swift_windows() {
        assert_eq!(start_path(BridgeWindow::Main, None), "index.html#/main");
        assert_eq!(
            start_path(BridgeWindow::Settings, Some("section=summaries")),
            "index.html#/settings?section=summaries"
        );
        assert_eq!(
            start_path(BridgeWindow::Onboarding, Some("")),
            "index.html#/onboarding"
        );
    }

    #[test]
    fn sizes_follow_the_swift_windows() {
        assert_eq!(Spec::of(BridgeWindow::Main).size, (1120.0, 720.0));
        assert_eq!(Spec::of(BridgeWindow::Main).min_size, Some((960.0, 600.0)));
        assert_eq!(Spec::of(BridgeWindow::Settings).size, (960.0, 640.0));
        assert_eq!(
            Spec::of(BridgeWindow::Settings).min_size,
            Some((760.0, 520.0))
        );
        assert_eq!(Spec::of(BridgeWindow::Onboarding).size, (560.0, 620.0));
        assert_eq!(Spec::of(BridgeWindow::Onboarding).min_size, None);
    }

    /// The field names are the contract's: an `AppSnapshot` with both
    /// requests set writes exactly these keys beside its own.
    #[test]
    fn the_request_fields_are_the_app_snapshots() {
        let id = steno_core::json::parse_uuid("00000000-0000-0000-0000-00000000000c").unwrap();
        let mut app: steno_bridge::AppSnapshot =
            serde_json::from_str(include_str!("../../../macos/web/fixtures/bridge/app.json"))
                .unwrap();
        let bare = serde_json::to_value(&app).unwrap();
        app.requested_meeting_id = Some(id);
        app.requested_settings_section = Some(steno_bridge::SettingsSection::Export);
        let requesting = serde_json::to_value(&app).unwrap();
        let added: Vec<&str> = requesting
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| bare.get(key.as_str()).is_none())
            .map(String::as_str)
            .collect();
        let mut fields: Vec<&str> = RequestField::ALL
            .iter()
            .map(|field| field.as_str())
            .collect();
        fields.sort_unstable();
        assert_eq!(added, fields);
        assert_eq!(
            requesting[RequestField::MeetingId.as_str()],
            Request::meeting(&id).value
        );
        assert_eq!(
            requesting[RequestField::SettingsSection.as_str()],
            Request::section(steno_bridge::SettingsSection::Export).value
        );
    }

    /// Only Settings and onboarding are kept on a close, and only on Linux.
    #[test]
    fn settings_and_onboarding_are_kept_on_linux_only() {
        let linux = cfg!(target_os = "linux");
        assert_eq!(retires_on_close("settings"), linux);
        assert_eq!(retires_on_close("onboarding"), linux);
        for label in ["main", "prompt", "bubble", "other"] {
            assert!(!retires_on_close(label), "{label}");
        }
    }

    /// A kept window is noted once, opens again once, and is forgotten
    /// with its document.
    #[test]
    fn a_kept_window_opens_again_once() {
        let retired = Retired::default();
        let document = Url::parse("tauri://localhost/index.html#/settings?section=export").unwrap();
        assert!(!retired.holds("settings"));
        assert!(retired.note("settings", document.clone()));
        let blank = Url::parse("about:blank").unwrap();
        assert!(!retired.note("settings", blank), "kept twice");
        assert!(retired.holds("settings"));
        assert!(!retired.holds("onboarding"));
        assert_eq!(retired.revive("settings"), Some(document.clone()));
        assert_eq!(retired.revive("settings"), None);
        assert!(!retired.forget("settings"));
        assert!(retired.note("onboarding", document));
        assert!(retired.forget("onboarding"));
        assert!(!retired.holds("onboarding"));
    }

    /// A kept window loads its own route with the new query on the
    /// document it showed, whatever route and query it showed last.
    #[test]
    fn a_kept_window_reopens_on_its_route_with_the_new_query() {
        let shown = Url::parse("tauri://localhost/index.html#/settings?section=export").unwrap();
        assert_eq!(
            reopened(&shown, BridgeWindow::Settings, None).as_str(),
            "tauri://localhost/index.html#/settings"
        );
        assert_eq!(
            reopened(&shown, BridgeWindow::Settings, Some("section=summaries")).as_str(),
            "tauri://localhost/index.html#/settings?section=summaries"
        );
        let dev = Url::parse("http://localhost:5173/#/onboarding").unwrap();
        assert_eq!(
            reopened(&dev, BridgeWindow::Onboarding, None).as_str(),
            "http://localhost:5173/#/onboarding"
        );
    }

    /// A request for a page that has not mounted waits for its
    /// `page.ready`; one for a page that has goes out at once; a destroyed
    /// window forgets both.
    #[test]
    fn a_request_waits_for_the_page_then_goes_out() {
        let pages = Pages::default();
        let id = steno_core::json::parse_uuid("00000000-0000-0000-0000-00000000000c").unwrap();
        assert!(!pages.is_ready("main"));
        assert_eq!(pages.publish_or_owe("main", Request::meeting(&id)), None);
        // Only the latest request is owed.
        let export = Request::section(steno_bridge::SettingsSection::Export);
        assert_eq!(pages.publish_or_owe("main", export.clone()), None);
        assert!(!pages.is_ready("main"));
        assert_eq!(
            pages.ready("main"),
            Some(Request {
                field: RequestField::SettingsSection,
                value: "export".into(),
            })
        );
        assert!(pages.is_ready("main"));
        // A reload mounts again and is owed nothing.
        assert_eq!(pages.ready("main"), None);
        assert!(pages.is_ready("main"));
        // A page that listens gets the request back to publish at once,
        // and owes nothing for its next mount.
        assert_eq!(pages.publish_or_owe("main", export.clone()), Some(export));
        assert_eq!(pages.ready("main"), None);
        // The other windows are their own.
        assert!(!pages.is_ready("settings"));
        assert_eq!(pages.ready("settings"), None);
        assert_eq!(
            pages.publish_or_owe("onboarding", Request::meeting(&id)),
            None
        );
        pages.gone("onboarding");
        assert!(!pages.is_ready("onboarding"));
        assert_eq!(pages.ready("onboarding"), None);
        assert_eq!(
            Request::meeting(&id),
            Request {
                field: RequestField::MeetingId,
                value: "00000000-0000-0000-0000-00000000000C".into(),
            }
        );
    }
}
