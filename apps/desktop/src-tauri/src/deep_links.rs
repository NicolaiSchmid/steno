//! The `steno:` URL scheme. The Swift Mac app registers none (`project.yml`
//! has no `CFBundleURLTypes`); `steno://pair/v1?…` is the iPhone app's,
//! the link the pairing QR code doubles as (`PairingPayload.swift`). The
//! shell registers the scheme on every platform (`plugins.deep-link` in
//! `tauri.conf.json`) and answers two links of its own, which land as the
//! `app` snapshot's requests, the way `window.open` does:
//!
//! | Link | Effect |
//! |---|---|
//! | `steno://meeting/<uuid>` | opens the main window on that meeting (`requestedMeetingID`) |
//! | `steno://settings[/<section>]` | opens Settings on the section (`requestedSettingsSection`) |
//!
//! A pairing link is logged and ignored: the Mac is the host, not the
//! phone. Anything else is logged and ignored too. A second instance
//! started with a link hands it to the first (`tauri-plugin-single-instance`
//! with its `deep-link` feature) and exits.

use tauri::{AppHandle, Manager, Url};
use tauri_plugin_deep_link::DeepLinkExt;
use uuid::Uuid;

use crate::{
    bridge::{SettingsSection, WindowParams},
    host::Host,
    windows::{self, BridgeWindow},
};

pub const SCHEME: &str = "steno";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepLink {
    Meeting(Uuid),
    Settings(Option<SettingsSection>),
    /// `steno://pair/…`: the iPhone's link.
    Pair,
}

/// Why a URL is not a link the shell follows.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeepLinkError {
    #[error("not a {SCHEME}: link: {0}")]
    OtherScheme(String),
    #[error("not a meeting id: {0}")]
    NotAMeeting(String),
    #[error("not a Settings section: {0}")]
    NotASection(String),
    #[error("unknown link: {0}")]
    Unknown(String),
}

impl DeepLink {
    /// Reads a link. The host part is the verb (`steno://meeting/…`), as
    /// the pairing link is written; a path-only form (`steno:/meeting/…`)
    /// is not accepted.
    pub fn parse(url: &Url) -> Result<Self, DeepLinkError> {
        if url.scheme() != SCHEME {
            return Err(DeepLinkError::OtherScheme(url.to_string()));
        }
        let path = url.path().trim_matches('/');
        match url.host_str() {
            Some("meeting") => {
                // The hyphenated 36-character form only, as `UUID(uuidString:)`
                // and the `window.open` params read it.
                (path.len() == 36)
                    .then(|| Uuid::try_parse(path).ok())
                    .flatten()
                    .map(Self::Meeting)
                    .ok_or_else(|| DeepLinkError::NotAMeeting(path.to_owned()))
            }
            Some("settings") => {
                if path.is_empty() {
                    return Ok(Self::Settings(None));
                }
                serde_json::from_value::<SettingsSection>(serde_json::Value::String(
                    path.to_owned(),
                ))
                .map(|section| Self::Settings(Some(section)))
                .map_err(|_| DeepLinkError::NotASection(path.to_owned()))
            }
            Some("pair") => Ok(Self::Pair),
            _ => Err(DeepLinkError::Unknown(url.to_string())),
        }
    }

    /// The `window.open` request the link amounts to; `None` for the
    /// phone's link.
    pub fn window_request(&self) -> Option<WindowParams> {
        match self {
            Self::Meeting(id) => Some(WindowParams {
                window: BridgeWindow::Main,
                section: None,
                meeting_id: Some(*id),
            }),
            Self::Settings(section) => Some(WindowParams {
                window: BridgeWindow::Settings,
                section: *section,
                meeting_id: None,
            }),
            Self::Pair => None,
        }
    }
}

/// Follows every link in `urls` that the shell answers.
pub fn handle(app: &AppHandle, urls: &[Url]) {
    for url in urls {
        match DeepLink::parse(url) {
            Ok(DeepLink::Pair) => {
                eprintln!("[steno-desktop] {url} is the iPhone's pairing link; nothing to do here");
            }
            Ok(link) => {
                let Some(request) = link.window_request() else {
                    continue;
                };
                let host = app.state::<Host>();
                if let Err(error) = windows::open_requested(app, &host, &request) {
                    eprintln!("[steno-desktop] {url}: {error}");
                }
            }
            Err(error) => eprintln!("[steno-desktop] deep link ignored: {error}"),
        }
    }
}

/// Listens for links while the app runs and follows the one it may have
/// been started with. A debug build on Linux or Windows also registers
/// the scheme for the running binary, which the installers do for a
/// release (the `.desktop` file, the registry).
pub fn install(app: &AppHandle) {
    #[cfg(any(target_os = "linux", windows))]
    if cfg!(debug_assertions)
        && let Err(error) = app.deep_link().register_all()
    {
        eprintln!("[steno-desktop] registering {SCHEME}: failed: {error}");
    }
    let listener = app.clone();
    app.deep_link()
        .on_open_url(move |event| handle(&listener, &event.urls()));
    match app.deep_link().get_current() {
        Ok(Some(urls)) => handle(app, &urls),
        Ok(None) => {}
        Err(error) => eprintln!("[steno-desktop] reading the launch link failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(text: &str) -> Result<DeepLink, DeepLinkError> {
        DeepLink::parse(&Url::parse(text).unwrap())
    }

    #[test]
    fn a_meeting_link_opens_the_main_window_on_the_meeting() {
        let id: Uuid = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        let parsed = link("steno://meeting/00000000-0000-0000-0000-000000000001").unwrap();
        assert_eq!(parsed, DeepLink::Meeting(id));
        let request = parsed.window_request().unwrap();
        assert_eq!(request.window, BridgeWindow::Main);
        assert_eq!(request.meeting_id, Some(id));
        assert_eq!(request.section, None);
        // Either case, as `UUID(uuidString:)` reads it; a trailing slash is fine.
        assert_eq!(
            link("steno://meeting/00000000-0000-0000-0000-00000000000A/").unwrap(),
            DeepLink::Meeting("00000000-0000-0000-0000-00000000000a".parse().unwrap())
        );
    }

    #[test]
    fn a_settings_link_opens_settings_on_the_section() {
        assert_eq!(link("steno://settings").unwrap(), DeepLink::Settings(None));
        assert_eq!(link("steno://settings/").unwrap(), DeepLink::Settings(None));
        let parsed = link("steno://settings/summaries").unwrap();
        assert_eq!(parsed, DeepLink::Settings(Some(SettingsSection::Summaries)));
        let request = parsed.window_request().unwrap();
        assert_eq!(request.window, BridgeWindow::Settings);
        assert_eq!(request.section, Some(SettingsSection::Summaries));
        assert_eq!(request.meeting_id, None);
    }

    #[test]
    fn the_phones_pairing_link_is_recognised_and_not_followed() {
        let parsed = link(
            "steno://pair/v1?mac=00000000-0000-0000-0000-000000000001&name=Mac&fp=x&secret=y&exp=1",
        )
        .unwrap();
        assert_eq!(parsed, DeepLink::Pair);
        assert_eq!(parsed.window_request(), None);
    }

    #[test]
    fn everything_else_is_an_error_that_names_the_problem() {
        assert_eq!(
            link("https://example.com/meeting/1"),
            Err(DeepLinkError::OtherScheme(
                "https://example.com/meeting/1".into()
            ))
        );
        assert_eq!(
            link("steno://meeting/m-1"),
            Err(DeepLinkError::NotAMeeting("m-1".into()))
        );
        assert_eq!(
            link("steno://meeting/0000000000000000000000000000000c"),
            Err(DeepLinkError::NotAMeeting(
                "0000000000000000000000000000000c".into()
            ))
        );
        assert_eq!(
            link("steno://settings/advanced"),
            Err(DeepLinkError::NotASection("advanced".into()))
        );
        assert_eq!(
            link("steno://settings/General"),
            Err(DeepLinkError::NotASection("General".into()))
        );
        assert!(matches!(
            link("steno://record"),
            Err(DeepLinkError::Unknown(_))
        ));
        assert!(matches!(
            link("steno:meeting"),
            Err(DeepLinkError::Unknown(_))
        ));
        assert_eq!(
            link("steno://meeting/m-1").unwrap_err().to_string(),
            "not a meeting id: m-1"
        );
    }

    #[test]
    fn the_scheme_is_the_one_the_config_registers() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let schemes = &config["plugins"]["deep-link"]["desktop"]["schemes"];
        assert_eq!(schemes, &serde_json::json!([SCHEME]));
    }
}
