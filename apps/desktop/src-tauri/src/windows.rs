//! The three windows: routes, sizes and minimums. Main is hidden, not
//! destroyed, on close; Settings and onboarding are created on
//! `window.open`. On Linux those two are kept hidden on `about:blank`
//! instead of destroyed (`Kept`), since every destroyed `WebKitGTK` view
//! leaks a file descriptor (#160). Requests for an unmounted page wait in
//! `Pages`.

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
    host::{self, ShellHost},
    navigation,
};

/// One window's size in logical points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spec {
    pub title: &'static str,
    pub route: &'static str,
    pub size: (f64, f64),
    /// `None` for a window of fixed size.
    pub min_size: Option<(f64, f64)>,
}

impl Spec {
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

/// The document a window loads: `index.html` plus the hash route and query.
pub fn start_path(window: BridgeWindow, query: Option<&str>) -> String {
    format!("index.html{}", with_query(Spec::of(window).route, query))
}

pub fn with_query(route: &str, query: Option<&str>) -> String {
    match query {
        Some(query) if !query.is_empty() => format!("{route}?{query}"),
        _ => route.to_owned(),
    }
}

/// Shows and focuses the window when it exists, creates it otherwise, at
/// `position` or where the system places it.
pub fn open(
    app: &AppHandle,
    window: BridgeWindow,
    query: Option<&str>,
    position: Option<(f64, f64)>,
) -> tauri::Result<WebviewWindow> {
    let spec = Spec::of(window);
    if let Some(existing) = app.get_webview_window(window.as_str()) {
        let kept = app.state::<Kept>();
        // Forgotten only once the navigation went out, so a failure retries.
        if let Some(document) = kept.document(window.as_str()) {
            existing.navigate(reopened(&document, window, query))?;
            kept.forget(window.as_str());
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
    // Links that open a new window open nothing: external links go through
    // `system.openURL`. The explicit `Deny` pins wry's default.
    .on_new_window(|_url, _features| NewWindowResponse::Deny);
    if let Some((width, height)) = spec.min_size {
        builder = builder.min_inner_size(width, height);
    }
    if let Some((x, y)) = position {
        builder = builder.position(x, y);
    }
    // The page paints up to the top edge around the traffic lights; the
    // page's `TITLE_BAR_INSET` follows this `cfg`.
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
    }
    builder.build()
}

/// `window.open` from a page, or a deep link: opens or focuses the window.
/// A meeting, or a section for an existing Settings window, rides on the
/// window's next `app` snapshot once its page listens (`Pages`); a new
/// Settings window reads the section from its route.
pub fn open_requested(app: &AppHandle, params: &WindowParams) -> Result<(), BridgeError> {
    let pages = app.state::<Pages>();
    let existed = app.get_webview_window(params.window.as_str()).is_some()
        && app
            .state::<Kept>()
            .document(params.window.as_str())
            .is_none();
    let asked = match params.window {
        BridgeWindow::Onboarding => {
            open(app, BridgeWindow::Onboarding, None, None).map_err(failed)?;
            None
        }
        BridgeWindow::Main => {
            let window = open(app, BridgeWindow::Main, None, None).map_err(failed)?;
            params
                .meeting_id
                .map(|meeting_id| (window, Request::meeting(&meeting_id)))
        }
        BridgeWindow::Settings => {
            let section = params.section;
            let query = section.map(|section| format!("section={section}"));
            let window =
                open(app, BridgeWindow::Settings, query.as_deref(), None).map_err(failed)?;
            section
                .filter(|_| existed)
                .map(|section| (window, Request::section(section)))
        }
    };
    if let Some((window, asked)) = asked {
        request(app, &pages, window.label(), window.clone(), asked);
    }
    Ok(())
}

steno_core::string_enum! {
    /// The `app` snapshot's two request fields; a test pins the names to
    /// `AppSnapshot`'s.
    pub enum RequestField {
        MeetingId = "requestedMeetingID",
        SettingsSection = "requestedSettingsSection",
    }
}

/// A meeting or a section for a window: the request field and its value.
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
/// owed. A request published before the page listens would be lost (a
/// deep link at a cold launch), so it waits here. One lock covers both,
/// and requests are handed to [`publish`] under it, so they are queued in
/// the order they came, an owed one first. Managed state.
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
    /// The page of `label` sent `page.ready`: its owed request, if any,
    /// goes to `publish` under the lock.
    pub fn ready(&self, label: &str, publish: impl FnOnce(Request)) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.ready.insert(label.to_owned());
        if let Some(owed) = state.owed.remove(label) {
            publish(owed);
        }
    }

    #[cfg(test)]
    fn is_ready(&self, label: &str) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| state.ready.contains(label))
    }

    /// Hands `request` to `publish` under the lock when the page listens,
    /// else keeps it for its `page.ready`, replacing an earlier one.
    pub fn publish_or_owe(&self, label: &str, request: Request, publish: impl FnOnce(Request)) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.ready.contains(label) {
            publish(request);
        } else {
            state.owed.insert(label.to_owned(), request);
        }
    }

    /// The window of `label` was destroyed, and with it what it was owed.
    pub fn gone(&self, label: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.ready.remove(label);
            state.owed.remove(label);
        }
    }
}

/// Publishes `request` to `window` when its page listens, else owes it.
pub fn request<H: ShellHost>(
    host: &H,
    pages: &Pages,
    label: &str,
    window: H::Window,
    request: Request,
) {
    pages.publish_or_owe(label, request, |now| publish(host, window, now));
}

/// The page of `window` has mounted: publishes its owed request, if any.
pub fn publish_owed<H: ShellHost>(host: &H, pages: &Pages, label: &str, window: H::Window) {
    pages.ready(label, |owed| publish(host, window, owed));
}

/// Queues `request`'s publish on the shell's ordered lane and returns:
/// selecting a meeting reads the store, and the caller may be the main
/// thread. The lane keeps requests in order, so the later of two deep
/// links is the one shown.
pub fn publish<H: ShellHost>(host: &H, window: H::Window, request: Request) {
    host::call_in_order(host.clone(), "a window request", move |host| {
        host.publish_request(&window, request.field.as_str(), &request.value)
    });
}

/// Whether closing the window of `label` keeps it instead (`keep`):
/// Settings and onboarding on Linux.
pub fn keeps_on_close(label: &str) -> bool {
    cfg!(target_os = "linux")
        && (label == BridgeWindow::Settings.as_str() || label == BridgeWindow::Onboarding.as_str())
}

/// The kept windows, each with the app document it showed. Managed state.
#[derive(Debug, Default)]
pub struct Kept(Mutex<HashMap<String, Url>>);

impl Kept {
    /// Notes `label` as kept, showing `document`; false when it already
    /// was, which keeps the first document.
    fn note(&self, label: &str, document: Url) -> bool {
        let Ok(mut kept) = self.0.lock() else {
            return false;
        };
        if kept.contains_key(label) {
            return false;
        }
        kept.insert(label.to_owned(), document);
        true
    }

    fn document(&self, label: &str) -> Option<Url> {
        self.0.lock().ok()?.get(label).cloned()
    }

    /// Stops keeping `label`; whether it was kept.
    pub fn forget(&self, label: &str) -> bool {
        self.0
            .lock()
            .is_ok_and(|mut kept| kept.remove(label).is_some())
    }
}

/// Keeps `window` after its close: forgets its page, hides it and loads
/// `about:blank`. False when it was kept already. On an error it is not
/// kept, and the caller destroys it.
pub fn keep(app: &AppHandle, window: &WebviewWindow) -> tauri::Result<bool> {
    let label = window.label();
    let main = || {
        app.get_webview_window(BridgeWindow::Main.as_str())
            .and_then(|main| main.url().ok())
    };
    let document = app_document(window.url()?, main)
        .ok_or_else(|| std::io::Error::other("the window shows no document of the app"))?;
    let kept = app.state::<Kept>();
    if !kept.note(label, document) {
        return Ok(false);
    }
    app.state::<Pages>().gone(label);
    let hidden = window
        .hide()
        .and_then(|()| window.navigate(Url::parse("about:blank").expect("a URL")));
    if let Err(error) = hidden {
        kept.forget(label);
        return Err(error);
    }
    Ok(true)
}

/// The app document to remember for a window showing `shown`: `shown`
/// itself, or main's when it shows `about:blank`; none when neither is
/// the app's.
fn app_document(shown: Url, main: impl FnOnce() -> Option<Url>) -> Option<Url> {
    let blank = |url: &Url| url.as_str() == "about:blank";
    if blank(&shown) {
        main().filter(|main| !blank(main))
    } else {
        Some(shown)
    }
}

/// What a kept window loads when reopened: its document with `window`'s
/// route and `query`, so the page mounts afresh.
fn reopened(document: &Url, window: BridgeWindow, query: Option<&str>) -> Url {
    let mut url = document.clone();
    let route = Spec::of(window).route.trim_start_matches('#');
    url.set_fragment(Some(&with_query(route, query)));
    url
}

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

    /// The field names match the keys `AppSnapshot` writes.
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
        assert_eq!(keeps_on_close("settings"), linux);
        assert_eq!(keeps_on_close("onboarding"), linux);
        for label in ["main", "prompt", "bubble", "other"] {
            assert!(!keeps_on_close(label), "{label}");
        }
    }

    /// A kept window is noted once and kept anew after it is forgotten.
    #[test]
    fn a_kept_window_is_noted_once_until_it_is_forgotten() {
        let kept = Kept::default();
        let document = Url::parse("tauri://localhost/index.html#/settings?section=export").unwrap();
        assert_eq!(kept.document("settings"), None);
        assert!(kept.note("settings", document.clone()));
        let blank = Url::parse("about:blank").unwrap();
        assert!(!kept.note("settings", blank), "kept twice");
        assert_eq!(kept.document("settings"), Some(document.clone()));
        assert_eq!(kept.document("settings"), Some(document.clone()));
        assert_eq!(kept.document("onboarding"), None);
        assert!(kept.forget("settings"));
        assert_eq!(kept.document("settings"), None);
        assert!(!kept.forget("settings"));
        assert!(kept.note("onboarding", document));
        assert!(kept.forget("onboarding"));
        assert_eq!(kept.document("onboarding"), None);
    }

    #[test]
    fn a_kept_window_keeps_an_app_document() {
        let url = |text: &str| Url::parse(text).unwrap();
        let settings = url("tauri://localhost/index.html#/settings");
        let main = url("tauri://localhost/index.html#/main");
        let blank = url("about:blank");
        assert_eq!(
            app_document(settings.clone(), || panic!("main was asked")),
            Some(settings)
        );
        assert_eq!(
            app_document(blank.clone(), || Some(main.clone())),
            Some(main)
        );
        assert_eq!(app_document(blank.clone(), || None), None);
        assert_eq!(app_document(blank.clone(), || Some(blank.clone())), None);
    }

    /// A reopened window loads its own route with the new query.
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

    fn ready(pages: &Pages, label: &str) -> Option<Request> {
        let mut published = None;
        pages.ready(label, |owed| published = Some(owed));
        published
    }

    fn publish_or_owe(pages: &Pages, label: &str, request: Request) -> Option<Request> {
        let mut published = None;
        pages.publish_or_owe(label, request, |now| published = Some(now));
        published
    }

    /// A request waits for the page's `page.ready`, goes out at once once
    /// it listens, and is dropped with a destroyed window.
    #[test]
    fn a_request_waits_for_the_page_then_goes_out() {
        let pages = Pages::default();
        let id = steno_core::json::parse_uuid("00000000-0000-0000-0000-00000000000c").unwrap();
        assert!(!pages.is_ready("main"));
        assert_eq!(publish_or_owe(&pages, "main", Request::meeting(&id)), None);
        // Only the latest request is owed.
        let export = Request::section(steno_bridge::SettingsSection::Export);
        assert_eq!(publish_or_owe(&pages, "main", export.clone()), None);
        assert!(!pages.is_ready("main"));
        assert_eq!(
            ready(&pages, "main"),
            Some(Request {
                field: RequestField::SettingsSection,
                value: "export".into(),
            })
        );
        assert!(pages.is_ready("main"));
        assert_eq!(ready(&pages, "main"), None);
        assert!(pages.is_ready("main"));
        assert_eq!(publish_or_owe(&pages, "main", export.clone()), Some(export));
        assert_eq!(ready(&pages, "main"), None);
        assert!(!pages.is_ready("settings"));
        assert_eq!(ready(&pages, "settings"), None);
        assert_eq!(
            publish_or_owe(&pages, "onboarding", Request::meeting(&id)),
            None
        );
        pages.gone("onboarding");
        assert!(!pages.is_ready("onboarding"));
        assert_eq!(ready(&pages, "onboarding"), None);
        assert_eq!(
            Request::meeting(&id),
            Request {
                field: RequestField::MeetingId,
                value: "00000000-0000-0000-0000-00000000000C".into(),
            }
        );
    }

    /// A request for a listening page is queued on the lane without
    /// blocking the caller; one for an unmounted page reaches no host.
    #[test]
    fn a_window_request_is_published_off_the_callers_thread() {
        use std::time::Duration;

        use crate::host::gated::{GatedHost, returns_at_once};

        let meeting = Request {
            field: RequestField::MeetingId,
            value: "m-1".to_owned(),
        };
        let held = "request requestedMeetingID=m-1";
        assert_eq!(
            format!("request {}={}", meeting.field.as_str(), meeting.value),
            held
        );
        let host = GatedHost::holding(held);
        let _opens = host.opens_on_drop();
        let pages = std::sync::Arc::new(Pages::default());
        request(&host, &pages, "settings", "settings", meeting.clone());
        assert_eq!(
            ready(&pages, "settings"),
            Some(meeting.clone()),
            "owed to the page"
        );
        assert!(ready(&pages, "main").is_none());
        let (clicked, listening) = (host.clone(), pages.clone());
        assert!(
            returns_at_once(move || request(&clicked, &listening, "main", "main", meeting)),
            "the caller waited for the publish"
        );
        assert!(
            host.wait_held(Duration::from_secs(5)),
            "the publish reached the host"
        );
        host.open();
        assert!(host.wait_answered(held, Duration::from_secs(5)));
        assert_eq!(host.answered(), [held]);
    }

    /// An owed request is queued at `page.ready`, before a later request,
    /// so the later one is the meeting shown.
    #[test]
    fn an_owed_request_goes_out_before_a_later_one() {
        use std::time::Duration;

        use crate::host::gated::GatedHost;

        let meeting = |value: &str| Request {
            field: RequestField::MeetingId,
            value: value.to_owned(),
        };
        let host = GatedHost::holding("recording.toggle");
        let _opens = host.opens_on_drop();
        let pages = Pages::default();
        request(&host, &pages, "main", "main", meeting("m-a"));
        crate::actions::record_through(host.clone(), "main", None);
        assert!(
            host.wait_held(Duration::from_secs(5)),
            "the stop reached the host"
        );
        publish_owed(&host, &pages, "main", "main");
        request(&host, &pages, "main", "main", meeting("m-b"));
        assert_eq!(
            host.answered(),
            Vec::<String>::new(),
            "all wait on the stop"
        );
        host.open();
        assert!(host.wait_answers(3, Duration::from_secs(5)));
        assert_eq!(
            host.answered(),
            [
                "recording.toggle",
                "request requestedMeetingID=m-a",
                "request requestedMeetingID=m-b",
            ]
        );
    }
}
