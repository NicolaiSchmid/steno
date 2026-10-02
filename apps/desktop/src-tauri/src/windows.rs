//! The three windows, with the Swift app's routes, sizes and minimums
//! (`MainWindow.swift`, `SettingsWindow.swift`, `OnboardingWindow.swift`).
//! Main opens at start; Settings and onboarding are created on
//! `window.open` and focused when they already exist.

use serde::Deserialize;
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::{
    bridge::{BridgeFailure, WindowParams},
    host::Host,
    navigation,
};

/// `params.window.json`'s `window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
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

impl Kind {
    pub const fn spec(self) -> Spec {
        match self {
            Kind::Main => Spec {
                label: "main",
                title: "Steno",
                route: "#/main",
                size: (1120.0, 720.0),
                min_size: Some((960.0, 600.0)),
            },
            Kind::Settings => Spec {
                label: "settings",
                title: "Settings",
                route: "#/settings",
                size: (960.0, 640.0),
                min_size: Some((760.0, 520.0)),
            },
            Kind::Onboarding => Spec {
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
pub fn start_path(kind: Kind, query: Option<&str>) -> String {
    let route = kind.spec().route;
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
    kind: Kind,
    query: Option<&str>,
    position: Option<(f64, f64)>,
) -> tauri::Result<WebviewWindow> {
    let spec = kind.spec();
    if let Some(existing) = app.get_webview_window(spec.label) {
        existing.show()?;
        existing.set_focus()?;
        return Ok(existing);
    }

    let dev_server: Option<Url> = if cfg!(debug_assertions) {
        app.config().build.dev_url.clone()
    } else {
        None
    };
    let mut builder = WebviewWindowBuilder::new(
        app,
        spec.label,
        WebviewUrl::App(start_path(kind, query).into()),
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

/// `window.open` from a page (`WindowRequests.swift`).
pub fn open_requested(
    app: &AppHandle,
    host: &Host,
    request: &WindowParams,
) -> Result<(), BridgeFailure> {
    let existed = app.get_webview_window(request.window.label()).is_some();
    match request.window {
        Kind::Onboarding => {
            open(app, Kind::Onboarding, None, None)?;
        }
        Kind::Main => {
            let window = open(app, Kind::Main, None, None)?;
            if let Some(meeting_id) = request.meeting_id.as_deref() {
                // The main window exists for the app's lifetime, so the
                // request always rides on its `app` snapshot.
                host.publish_request(&window, "requestedMeetingID", meeting_id)?;
            }
        }
        Kind::Settings => {
            // A new window reads the section from its route; an open one
            // learns it from the `app` snapshot.
            let section = request.section.as_deref();
            let query = section.map(|section| format!("section={section}"));
            let window = open(app, Kind::Settings, query.as_deref(), None)?;
            if let (true, Some(section)) = (existed, section) {
                host.publish_request(&window, "requestedSettingsSection", section)?;
            }
        }
    }
    Ok(())
}

/// Closes the window when it exists; nothing otherwise.
pub fn close(app: &AppHandle, kind: Kind) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(kind.label()) {
        window.close()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_follow_the_swift_windows() {
        assert_eq!(start_path(Kind::Main, None), "index.html#/main");
        assert_eq!(
            start_path(Kind::Settings, Some("section=summaries")),
            "index.html#/settings?section=summaries"
        );
        assert_eq!(
            start_path(Kind::Onboarding, Some("")),
            "index.html#/onboarding"
        );
    }

    #[test]
    fn sizes_follow_the_swift_windows() {
        assert_eq!(Kind::Main.spec().size, (1120.0, 720.0));
        assert_eq!(Kind::Main.spec().min_size, Some((960.0, 600.0)));
        assert_eq!(Kind::Settings.spec().size, (960.0, 640.0));
        assert_eq!(Kind::Settings.spec().min_size, Some((760.0, 520.0)));
        assert_eq!(Kind::Onboarding.spec().size, (560.0, 620.0));
        assert_eq!(Kind::Onboarding.spec().min_size, None);
    }

    #[test]
    fn window_params_read_the_recorded_shape() {
        let params: WindowParams =
            serde_json::from_str(r#"{"section":"summaries","window":"settings"}"#).unwrap();
        assert_eq!(params.window, Kind::Settings);
        assert_eq!(params.section.as_deref(), Some("summaries"));
        assert!(params.meeting_id.is_none());
        let main: WindowParams = serde_json::from_str(
            r#"{"window":"main","meetingID":"00000000-0000-0000-0000-000000000001"}"#,
        )
        .unwrap();
        assert_eq!(main.window, Kind::Main);
        assert!(serde_json::from_str::<WindowParams>(r#"{"window":"panel"}"#).is_err());
    }
}
