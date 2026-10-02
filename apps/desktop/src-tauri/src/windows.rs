//! The three windows: the Swift app's routes, sizes and minimums. Main opens
//! at start; Settings and onboarding are created on `window.open` and
//! focused when they already exist.
//!
//! Swift: the sizes live in `StenoApp.swift` (main), `SettingsWindow.swift`
//! and `OnboardingWindow.swift`, main's minimum in `MainWindow.swift`.

use serde::Deserialize;
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::{
    bridge::{BridgeError, WindowParams},
    host::Host,
    navigation,
};

/// `params.window.json`'s `window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BridgeWindow {
    Main,
    Settings,
    Onboarding,
}

/// One window as the Swift app sizes it, in logical points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spec {
    pub label: &'static str,
    pub title: &'static str,
    pub route: &'static str,
    pub size: (f64, f64),
    /// `None` for a window of fixed size.
    pub min_size: Option<(f64, f64)>,
}

impl BridgeWindow {
    pub const fn spec(self) -> Spec {
        match self {
            BridgeWindow::Main => Spec {
                label: "main",
                title: "Steno",
                route: "#/main",
                size: (1120.0, 720.0),
                min_size: Some((960.0, 600.0)),
            },
            BridgeWindow::Settings => Spec {
                label: "settings",
                title: "Settings",
                route: "#/settings",
                size: (960.0, 640.0),
                min_size: Some((760.0, 520.0)),
            },
            BridgeWindow::Onboarding => Spec {
                label: "onboarding",
                title: "Welcome to Steno",
                route: "#/onboarding",
                size: (560.0, 620.0),
                min_size: None,
            },
        }
    }

    pub fn label(self) -> &'static str {
        self.spec().label
    }
}

/// The document a window loads: `index.html` plus the hash route and its
/// query, resolved against the app origin by Tauri.
pub fn start_path(window: BridgeWindow, query: Option<&str>) -> String {
    let route = window.spec().route;
    match query {
        Some(query) if !query.is_empty() => format!("index.html{route}?{query}"),
        _ => format!("index.html{route}"),
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
    let spec = window.spec();
    if let Some(existing) = app.get_webview_window(spec.label) {
        existing.show()?;
        existing.set_focus()?;
        return Ok(existing);
    }

    // `dev` is Tauri's alias for a build without `custom-protocol`: the one
    // that loads `devUrl` instead of the embedded bundle, so the one that
    // may navigate there.
    let dev_server: Option<Url> = if cfg!(dev) {
        app.config().build.dev_url.clone()
    } else {
        None
    };
    let mut builder = WebviewWindowBuilder::new(
        app,
        spec.label,
        WebviewUrl::App(start_path(window, query).into()),
    )
    .title(spec.title)
    .inner_size(spec.size.0, spec.size.1)
    .resizable(spec.min_size.is_some())
    .on_navigation(move |url| navigation::allows(url, dev_server.as_ref()));
    if let Some((width, height)) = spec.min_size {
        builder = builder.min_inner_size(width, height);
    }
    if let Some((x, y)) = position {
        builder = builder.position(x, y);
    }
    // The Swift windows hide their title bar and let the page paint up to the
    // top edge, leaving the traffic lights their inset; the same look here.
    // Linux and Windows keep their native title bar until the design pass of
    // WP8 decides otherwise.
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
    }
    builder.build()
}

/// `window.open` from a page: opens or focuses the window. A meeting or a
/// section for a window that already exists rides on its next `app`
/// snapshot (`Host::publish_request`); a new Settings window reads the
/// section from its route.
///
/// Swift: `WindowRequests.swift`.
pub fn open_requested(
    app: &AppHandle,
    host: &Host,
    request: &WindowParams,
) -> Result<(), BridgeError> {
    let existed = app.get_webview_window(request.window.label()).is_some();
    match request.window {
        BridgeWindow::Onboarding => {
            open(app, BridgeWindow::Onboarding, None, None)?;
        }
        BridgeWindow::Main => {
            let window = open(app, BridgeWindow::Main, None, None)?;
            if let Some(meeting_id) = request.meeting_id.as_deref() {
                // The main window exists for the app's lifetime, so the
                // request always rides on its `app` snapshot.
                host.publish_request(&window, "requestedMeetingID", meeting_id)?;
            }
        }
        BridgeWindow::Settings => {
            let section = request.section.as_deref();
            let query = section.map(|section| format!("section={section}"));
            let window = open(app, BridgeWindow::Settings, query.as_deref(), None)?;
            if let (true, Some(section)) = (existed, section) {
                host.publish_request(&window, "requestedSettingsSection", section)?;
            }
        }
    }
    Ok(())
}

/// Closes the window when it exists; nothing otherwise.
pub fn close(app: &AppHandle, window: BridgeWindow) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(window.label()) {
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
        assert_eq!(BridgeWindow::Main.spec().size, (1120.0, 720.0));
        assert_eq!(BridgeWindow::Main.spec().min_size, Some((960.0, 600.0)));
        assert_eq!(BridgeWindow::Settings.spec().size, (960.0, 640.0));
        assert_eq!(BridgeWindow::Settings.spec().min_size, Some((760.0, 520.0)));
        assert_eq!(BridgeWindow::Onboarding.spec().size, (560.0, 620.0));
        assert_eq!(BridgeWindow::Onboarding.spec().min_size, None);
    }

    #[test]
    fn window_params_read_the_recorded_shape() {
        let params: WindowParams =
            serde_json::from_str(r#"{"section":"summaries","window":"settings"}"#).unwrap();
        assert_eq!(params.window, BridgeWindow::Settings);
        assert_eq!(params.section.as_deref(), Some("summaries"));
        assert!(params.meeting_id.is_none());
        let main: WindowParams = serde_json::from_str(
            r#"{"window":"main","meetingID":"00000000-0000-0000-0000-000000000001"}"#,
        )
        .unwrap();
        assert_eq!(main.window, BridgeWindow::Main);
        assert!(serde_json::from_str::<WindowParams>(r#"{"window":"panel"}"#).is_err());
    }
}
