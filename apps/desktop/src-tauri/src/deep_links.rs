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
//! A pairing link is logged (by its scheme and host only: its query holds
//! the pairing secret) and ignored: the Mac is the host, not the phone.
//! Anything else is logged and ignored too. A second instance
//! started with a link hands it to the first (`tauri-plugin-single-instance`
//! with its `deep-link` feature) and exits. Scheme and host are read
//! case-insensitively, as RFC 3986 has them; the path is read as written,
//! so a Settings section must be spelled as the contract spells it, while
//! a meeting id reads in either case, as `UUID(uuidString:)` does.
//!
//! On Linux the `.deb` registers the scheme through the desktop entry
//! (`linux/steno-desktop.desktop`: `Exec=… %u` and the
//! `x-scheme-handler/steno` MIME type). An `AppImage` carries the same
//! entry, but no system reads it, so an `AppImage` registers itself at
//! start, as a debug build does (`registers_itself`).
//!
//! Swift: `apps/macos/project.yml` (no `CFBundleURLTypes`: the Mac app
//! registers no scheme), `Sources/StenoHandover/Pairing/PairingPayload.swift`
//! (the phone's `steno://pair` link).

use steno_bridge::{SettingsSection, WindowParams};
use steno_core::json::parse_uuid;
use tauri::{AppHandle, Manager, Url};
use tauri_plugin_deep_link::DeepLinkExt;
use uuid::Uuid;

use crate::{
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

/// Why a URL is not a link the shell follows. Each names the link by its
/// scheme and host (`describe`) or by its path, never by its query: a
/// pairing link's query holds the pairing secret, and these end up in the
/// log.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeepLinkError {
    #[error("not a {SCHEME}: link: {0}")]
    OtherScheme(String),
    #[error("not a meeting id: {0}")]
    NotAMeeting(String),
    #[error("not a Settings section: {0}")]
    NotASection(String),
    #[error("{0} takes no query, fragment, user or port")]
    Extras(String),
    #[error("unknown link: {0}")]
    Unknown(String),
}

/// The link as the log may name it: scheme and host, nothing after.
pub fn describe(url: &Url) -> String {
    match url.host_str() {
        Some(host) => format!("{}://{host}", url.scheme()),
        None => format!("{}:", url.scheme()),
    }
}

/// The path's one segment: `""` for no path or `/`, `"x"` for `/x` and
/// `/x/`; `None` for anything else (an empty segment, a second one).
fn single_segment(path: &str) -> Option<&str> {
    if matches!(path, "" | "/") {
        return Some("");
    }
    let rest = path.strip_prefix('/')?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    (!rest.is_empty() && !rest.contains('/')).then_some(rest)
}

impl DeepLink {
    /// Reads a link. The host part is the verb (`steno://meeting/…`), as
    /// the pairing link is written; a path-only form (`steno:/meeting/…`)
    /// is not accepted. A meeting or Settings link is the verb and at most
    /// one path segment: a query, a fragment, a user, a port or an empty
    /// segment (`//`) make it no link the shell follows.
    pub fn parse(url: &Url) -> Result<Self, DeepLinkError> {
        if !url.scheme().eq_ignore_ascii_case(SCHEME) {
            return Err(DeepLinkError::OtherScheme(format!("{}:", url.scheme())));
        }
        let host = url.host_str().map(str::to_ascii_lowercase);
        let verb = match host.as_deref() {
            Some("pair") => return Ok(Self::Pair),
            Some(verb @ ("meeting" | "settings")) => verb,
            _ => return Err(DeepLinkError::Unknown(describe(url))),
        };
        if url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
        {
            return Err(DeepLinkError::Extras(describe(url)));
        }
        let path = url.path();
        let segment = single_segment(path);
        if verb == "meeting" {
            // The hyphenated 36-character form only, as `UUID(uuidString:)`
            // and the `window.open` params read it.
            return segment
                .and_then(parse_uuid)
                .map(Self::Meeting)
                .ok_or_else(|| DeepLinkError::NotAMeeting(segment.unwrap_or(path).to_owned()));
        }
        match segment {
            Some("") => Ok(Self::Settings(None)),
            Some(name) => name
                .parse::<SettingsSection>()
                .map(|section| Self::Settings(Some(section)))
                .map_err(|_| DeepLinkError::NotASection(name.to_owned())),
            None => Err(DeepLinkError::NotASection(path.to_owned())),
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
            Ok(link) => match link.window_request() {
                Some(request) => {
                    let host = app.state::<Host>();
                    if let Err(error) = windows::open_requested(app, &host, &request) {
                        eprintln!("[steno-desktop] {}", not_followed(url, &error));
                    }
                }
                None => eprintln!("[steno-desktop] {}", pairing_notice(url)),
            },
            Err(error) => eprintln!("[steno-desktop] deep link ignored: {error}"),
        }
    }
}

/// The log line for the phone's pairing link, named by scheme and host
/// alone: its query carries the pairing secret.
fn pairing_notice(url: &Url) -> String {
    format!(
        "{} is the iPhone's pairing link; nothing to do here",
        describe(url)
    )
}

/// The log line for a link whose window did not open, named by scheme and
/// host alone.
fn not_followed(url: &Url, error: &impl std::fmt::Display) -> String {
    format!("{}: {error}", describe(url))
}

/// Whether the running binary registers the scheme itself on Linux or
/// Windows: a debug build, which no installer put in place, and an
/// `AppImage`, whose embedded desktop entry no system reads (the plugin
/// writes one to the data directory that runs the `AppImage`). An
/// installed `.deb`, `.msi` or NSIS build has its desktop entry or
/// registry key from the installer.
#[cfg(any(target_os = "linux", windows, test))]
pub const fn registers_itself(debug_build: bool, appimage: bool) -> bool {
    debug_build || appimage
}

/// Listens for links while the app runs and follows the one it may have
/// been started with, registering the scheme first where the binary must
/// (`registers_itself`).
pub fn install(app: &AppHandle) {
    #[cfg(any(target_os = "linux", windows))]
    {
        #[cfg(target_os = "linux")]
        let appimage = app.env().appimage.is_some();
        #[cfg(windows)]
        let appimage = false;
        if registers_itself(cfg!(debug_assertions), appimage)
            && let Err(error) = app.deep_link().register_all()
        {
            eprintln!("[steno-desktop] registering {SCHEME}: failed: {error}");
        }
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
            link("https://example.com/meeting/1?token=x"),
            Err(DeepLinkError::OtherScheme("https:".into()))
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
        assert_eq!(
            link("steno://record?x=1"),
            Err(DeepLinkError::Unknown("steno://record".into()))
        );
        assert!(matches!(
            link("steno:meeting"),
            Err(DeepLinkError::Unknown(_))
        ));
        assert_eq!(
            link("steno://meeting/m-1").unwrap_err().to_string(),
            "not a meeting id: m-1"
        );
    }

    /// A meeting or Settings link is the verb and one segment: anything a
    /// URL can carry beside that is refused, named without its query.
    #[test]
    fn a_link_carries_nothing_beyond_its_verb_and_segment() {
        let id = "00000000-0000-0000-0000-000000000001";
        for text in [
            format!("steno://meeting/{id}?select=1"),
            format!("steno://meeting/{id}#notes"),
            format!("steno://user@meeting/{id}"),
            format!("steno://user:pass@meeting/{id}"),
            format!("steno://:pass@meeting/{id}"),
            format!("steno://meeting:80/{id}"),
            "steno://settings/summaries?secret=x".into(),
            "steno://settings?x".into(),
        ] {
            let error = link(&text).expect_err(&text);
            assert!(
                matches!(error, DeepLinkError::Extras(_)),
                "{text}: {error:?}"
            );
            let message = error.to_string();
            assert!(
                !message.contains("secret") && !message.contains("pass"),
                "{message}"
            );
        }
        assert_eq!(
            link(&format!("steno://meeting//{id}")),
            Err(DeepLinkError::NotAMeeting(format!("//{id}")))
        );
        assert_eq!(
            link(&format!("steno://meeting/{id}/notes")),
            Err(DeepLinkError::NotAMeeting(format!("/{id}/notes")))
        );
        assert_eq!(
            link(&format!("steno://meeting/{id}//")),
            Err(DeepLinkError::NotAMeeting(format!("/{id}//")))
        );
        assert_eq!(
            link("steno://meeting"),
            Err(DeepLinkError::NotAMeeting(String::new()))
        );
        assert_eq!(
            link("steno://settings//"),
            Err(DeepLinkError::NotASection("//".into()))
        );
        assert_eq!(
            link("steno://settings/export/x"),
            Err(DeepLinkError::NotASection("/export/x".into()))
        );
    }

    /// The pairing link's secret is in its query; the log names the link
    /// by its scheme and host alone.
    #[test]
    fn a_link_is_named_by_scheme_and_host_only() {
        let pair = Url::parse(
            "steno://pair/v1?mac=00000000-0000-0000-0000-000000000001&name=Mac&fp=x&secret=hunter2&exp=1",
        )
        .unwrap();
        assert_eq!(describe(&pair), "steno://pair");
        assert_eq!(
            pairing_notice(&pair),
            "steno://pair is the iPhone's pairing link; nothing to do here"
        );
        let meeting =
            Url::parse("steno://meeting/00000000-0000-0000-0000-000000000001?secret=x").unwrap();
        assert_eq!(
            not_followed(&meeting, &"no window"),
            "steno://meeting: no window"
        );
        assert_eq!(
            describe(&Url::parse("https://example.com/a?b=c").unwrap()),
            "https://example.com"
        );
        assert_eq!(describe(&Url::parse("steno:meeting").unwrap()), "steno:");
    }

    #[test]
    fn a_debug_build_and_an_appimage_register_the_scheme_themselves() {
        assert!(registers_itself(true, false));
        assert!(registers_itself(false, true));
        assert!(registers_itself(true, true));
        assert!(!registers_itself(false, false));
    }

    /// RFC 3986: the scheme and the host are case-insensitive; the path is
    /// not, except that a UUID reads in either case.
    #[test]
    fn scheme_and_host_read_in_any_case_and_the_path_as_written() {
        let id: Uuid = "00000000-0000-0000-0000-00000000000a".parse().unwrap();
        assert_eq!(
            link("STENO://Meeting/00000000-0000-0000-0000-00000000000A").unwrap(),
            DeepLink::Meeting(id)
        );
        assert_eq!(
            link("Steno://SETTINGS/summaries").unwrap(),
            DeepLink::Settings(Some(SettingsSection::Summaries))
        );
        assert_eq!(link("steno://PAIR/v1").unwrap(), DeepLink::Pair);
        assert_eq!(
            link("steno://settings/Summaries"),
            Err(DeepLinkError::NotASection("Summaries".into()))
        );
    }

    /// The desktop entry the Linux installers carry hands a `steno:` link
    /// to the running binary (`%u`) and claims the scheme. Read by line, so
    /// a checkout with CRLF endings (Windows CI) reads the same entry.
    #[test]
    fn the_desktop_entry_claims_the_scheme_and_takes_the_link() {
        let entry = include_str!("../linux/steno-desktop.desktop");
        let has_line = |line: &str| entry.lines().any(|candidate| candidate == line);
        assert!(has_line("Exec={{exec}} %u"), "{entry}");
        assert!(
            has_line(&format!("MimeType=x-scheme-handler/{SCHEME};")),
            "{entry}"
        );
        assert!(entry.contains("Categories="), "{entry}");
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            config["bundle"]["linux"]["deb"]["desktopTemplate"],
            "linux/steno-desktop.desktop"
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
