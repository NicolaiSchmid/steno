//! The three windows: the Swift app's routes, sizes and minimums. Main opens
//! at start; Settings and onboarding are created on `window.open` and
//! focused when they already exist.
//!
//! Swift: the sizes live in `StenoApp.swift` (main), `SettingsWindow.swift`
//! and `OnboardingWindow.swift`, main's minimum in `MainWindow.swift`.

use std::fmt;

use serde::Deserialize;
use tauri::{
    AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    webview::NewWindowResponse,
};

use crate::{
    bridge::{self, BridgeError, WindowParams},
    host::Host,
    navigation,
};

/// `params.window.json`'s `window`; the raw value is also the window label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BridgeWindow {
    Main,
    Settings,
    Onboarding,
}

impl BridgeWindow {
    /// The raw value on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Settings => "settings",
            Self::Onboarding => "onboarding",
        }
    }
}

impl fmt::Display for BridgeWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
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

impl Spec {
    /// The shell's own knowledge of a window, kept off `BridgeWindow` so the
    /// enum can come from `steno-bridge` unchanged.
    pub const fn of(window: BridgeWindow) -> Spec {
        match window {
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
}

/// The document a window loads: `index.html` plus the hash route and its
/// query, resolved against the app origin by Tauri.
pub fn start_path(window: BridgeWindow, query: Option<&str>) -> String {
    let route = Spec::of(window).route;
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
    let spec = Spec::of(window);
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
    let failed = |error: tauri::Error| BridgeError::failed(error.to_string());
    let existed = app.get_webview_window(request.window.as_str()).is_some();
    match request.window {
        BridgeWindow::Onboarding => {
            open(app, BridgeWindow::Onboarding, None, None).map_err(failed)?;
        }
        BridgeWindow::Main => {
            let window = open(app, BridgeWindow::Main, None, None).map_err(failed)?;
            if let Some(meeting_id) = request.meeting_id {
                // The main window exists for the app's lifetime, so the
                // request always rides on its `app` snapshot.
                host.publish_request(
                    &window,
                    "requestedMeetingID",
                    &bridge::uuid_text(&meeting_id),
                )?;
            }
        }
        BridgeWindow::Settings => {
            let section = request.section;
            let query = section.map(|section| format!("section={section}"));
            let window =
                open(app, BridgeWindow::Settings, query.as_deref(), None).map_err(failed)?;
            if let (true, Some(section)) = (existed, section) {
                host.publish_request(&window, "requestedSettingsSection", section.as_str())?;
            }
        }
    }
    Ok(())
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

    #[test]
    fn the_label_is_the_wire_value() {
        for window in [
            BridgeWindow::Main,
            BridgeWindow::Settings,
            BridgeWindow::Onboarding,
        ] {
            assert_eq!(Spec::of(window).label, window.as_str());
            assert_eq!(window.to_string(), window.as_str());
        }
    }
}
