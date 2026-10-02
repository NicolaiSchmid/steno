//! The `bridge_call` command: the Tauri side of
//! `apps/macos/web/src/bridge/tauri-transport.ts`. The page sends
//! `{ method, params }` and gets the result or a `BridgeError` with the
//! contract's error codes (`Sources/StenoBridge`, `envelope.error.json`).
//! Window and URL methods are the shell's own; everything else goes to the
//! host.
//!
//! Swift: `BridgeRequestParams.swift`, `WindowRequests.swift`.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, EventTarget, Manager, State, Url, WebviewWindow};
use tauri_plugin_opener::OpenerExt;
use uuid::Uuid;

use crate::{
    host::Host,
    smoke::Smoke,
    windows::{self, BridgeWindow},
};

/// The event every snapshot travels on; the page listens for it scoped to
/// its own window.
pub const EVENT_NAME: &str = "steno:event";

/// The contract's error codes the shell raises, spelled as the page reads
/// them. The full set (`notFound` and `cancelled` as well) is the host's and
/// comes with `steno-bridge`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BridgeErrorCode {
    UnknownMethod,
    InvalidParams,
    Failed,
}

impl BridgeErrorCode {
    /// The raw value on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownMethod => "unknownMethod",
            Self::InvalidParams => "invalidParams",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for BridgeErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A rejected command, serialised as the contract's error envelope; the
/// transport turns it into the page's `BridgeError`.
#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct BridgeError {
    pub code: BridgeErrorCode,
    pub message: String,
}

impl BridgeError {
    pub fn new(code: BridgeErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn unknown_method(message: impl Into<String>) -> Self {
        Self::new(BridgeErrorCode::UnknownMethod, message)
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(BridgeErrorCode::InvalidParams, message)
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(BridgeErrorCode::Failed, message)
    }
}

/// `invalidParams` for a method whose params did not decode, named after
/// the method so the page's log says which.
fn invalid_params_for(method: &str, error: impl fmt::Display) -> BridgeError {
    BridgeError::invalid_params(format!("{method}: {error}"))
}

/// The event envelope (`envelope.event.json`). `topic` becomes the
/// `BridgeTopic` enum with `steno-bridge`.
#[derive(Debug, Clone, Serialize)]
struct BridgeEvent<'a> {
    topic: &'a str,
    payload: Value,
}

/// Publishes one topic's snapshot to one window, and to that window only;
/// the smoke run counts what reaches main. An `onboarding` snapshot that
/// says `finished` also closes the onboarding window: the page shows what
/// the host says and the host ends the window.
///
/// Swift: `OnboardingWindow.swift` (`onChange(of: finished)`).
pub fn emit(window: &WebviewWindow, topic: &str, payload: Value) -> Result<(), BridgeError> {
    let finished = finishes_onboarding(topic, &payload);
    window
        .emit_to(
            EventTarget::webview_window(window.label()),
            EVENT_NAME,
            BridgeEvent { topic, payload },
        )
        .map_err(|error| BridgeError::failed(error.to_string()))?;
    window.state::<Smoke>().note_snapshot(window.label());
    if finished {
        windows::close(window.app_handle(), BridgeWindow::Onboarding)
            .map_err(|error| BridgeError::failed(error.to_string()))?;
    }
    Ok(())
}

/// Whether a snapshot ends onboarding: the `onboarding` topic with
/// `finished` true.
pub fn finishes_onboarding(topic: &str, payload: &Value) -> bool {
    topic == "onboarding" && payload["finished"] == Value::Bool(true)
}

/// The URLs a page may open: `https:` and `mailto:` links, so a page cannot
/// open files or run scripts; anything else is `invalidParams`.
///
/// Swift: `BridgeSystemCommands.openURL` in `BridgeRequestParams.swift`.
pub fn openable_url(text: &str) -> Result<Url, BridgeError> {
    Url::parse(text)
        .ok()
        .filter(|url| matches!(url.scheme(), "https" | "mailto"))
        .ok_or_else(|| {
            BridgeError::invalid_params("Only https: and mailto: links open from the page.")
        })
}

/// `settingsSection` in `contract.ts`: the six Settings sections, as the
/// route's `section=` and the `app` snapshot's `requestedSettingsSection`
/// spell them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingsSection {
    General,
    Recording,
    Transcription,
    Summaries,
    Export,
    Iphone,
}

impl SettingsSection {
    /// The raw value on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Recording => "recording",
            Self::Transcription => "transcription",
            Self::Summaries => "summaries",
            Self::Export => "export",
            Self::Iphone => "iphone",
        }
    }
}

impl fmt::Display for SettingsSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `params.window.json`, typed as `windowParams` in `contract.ts`: a window,
/// an optional section for Settings and an optional meeting for main. A
/// section outside the six or a meeting ID that is not a UUID is
/// `invalidParams`, so what reaches a route or a snapshot needs no escaping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowParams {
    pub window: BridgeWindow,
    #[serde(default)]
    pub section: Option<SettingsSection>,
    #[serde(rename = "meetingID", default)]
    pub meeting_id: Option<Uuid>,
}

/// A UUID as the Swift host writes it (`UUID.uuidString`, upper case), so a
/// `requestedMeetingID` matches the list's ids by string. `steno-bridge`
/// brings this as `json::uuid::format`.
pub fn uuid_text(id: &Uuid) -> String {
    id.hyphenated()
        .encode_upper(&mut Uuid::encode_buffer())
        .to_string()
}

/// `params.system.openURL.json`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenUrlParams {
    url: String,
}

fn parse<T: for<'de> Deserialize<'de>>(method: &str, params: Value) -> Result<T, BridgeError> {
    serde_json::from_value(params).map_err(|error| invalid_params_for(method, error))
}

/// Which window a `window.close` from `caller` may close: the onboarding
/// window, and only on its own request. The main and Settings windows do
/// not answer the method (`unknownMethod`, as their Swift hosts route it,
/// `MainWindowBridge.swift` and `SettingsBridge.swift`); the onboarding
/// window closes nothing but itself (`invalidParams`,
/// `OnboardingBridge.swift`). The page does not send the method today: the
/// host closes the window on a finished `onboarding` snapshot (`emit`).
pub fn close_target(caller: &str, request: &WindowParams) -> Result<BridgeWindow, BridgeError> {
    if caller != BridgeWindow::Onboarding.as_str() {
        return Err(BridgeError::unknown_method(format!(
            "The {caller} window does not answer window.close."
        )));
    }
    if request.window != BridgeWindow::Onboarding {
        return Err(BridgeError::invalid_params(
            "The onboarding window closes only itself.",
        ));
    }
    Ok(BridgeWindow::Onboarding)
}

#[tauri::command]
pub async fn bridge_call(
    app: AppHandle,
    window: WebviewWindow,
    host: State<'_, Host>,
    smoke: State<'_, Smoke>,
    method: String,
    params: Option<Value>,
) -> Result<Value, BridgeError> {
    let params = params.unwrap_or(Value::Null);
    match method.as_str() {
        "page.ready" => {
            smoke.note_ready(window.label());
            host.page_ready(&window)?;
            Ok(Value::Null)
        }
        "window.open" => {
            let request: WindowParams = parse(&method, params)?;
            windows::open_requested(&app, &host, &request)?;
            Ok(Value::Null)
        }
        "window.close" => {
            let request: WindowParams = parse(&method, params)?;
            let target = close_target(window.label(), &request)?;
            windows::close(&app, target).map_err(|error| BridgeError::failed(error.to_string()))?;
            Ok(Value::Null)
        }
        "system.openURL" => {
            let request: OpenUrlParams = parse(&method, params)?;
            let url = openable_url(&request.url)?;
            app.opener()
                .open_url(url, None::<&str>)
                .map_err(|error| BridgeError::failed(error.to_string()))?;
            Ok(Value::Null)
        }
        _ => host.call(&window, &method, params),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFUSAL: &str = "Only https: and mailto: links open from the page.";

    fn window_params(json: &str) -> Result<WindowParams, BridgeError> {
        parse("window.open", serde_json::from_str(json).unwrap())
    }

    #[test]
    fn https_and_mailto_links_open() {
        let github = openable_url("https://github.com/NicolaiSchmid/steno").unwrap();
        assert_eq!(github.host_str(), Some("github.com"));
        let mail = openable_url("mailto:hello@example.com").unwrap();
        assert_eq!(mail.path(), "hello@example.com");
        // The scheme is matched case-insensitively, as the Swift host does.
        assert!(openable_url("HTTPS://example.com/").is_ok());
    }

    #[test]
    fn every_other_scheme_is_invalid_params() {
        for text in [
            "http://example.com/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "steno://meeting/1",
            "ftp://example.com/",
        ] {
            let error = openable_url(text).expect_err(text);
            assert_eq!(error.code, BridgeErrorCode::InvalidParams, "{text}");
            assert_eq!(error.message, REFUSAL, "{text}");
        }
    }

    #[test]
    fn an_unparsable_url_is_invalid_params() {
        for text in ["", "not a url", "example.com/no-scheme", "https://"] {
            let error = openable_url(text).expect_err(text);
            assert_eq!(error.code, BridgeErrorCode::InvalidParams, "{text}");
            assert_eq!(error.message, REFUSAL, "{text}");
        }
    }

    #[test]
    fn the_error_serialises_as_the_contract_envelope() {
        let error = invalid_params_for("window.open", "missing field `window`");
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["code"], "invalidParams");
        assert_eq!(json["message"], "window.open: missing field `window`");
        // `Display` spells the code as the wire does, so this holds once the
        // types come from `steno-bridge`.
        assert_eq!(
            error.to_string(),
            "invalidParams: window.open: missing field `window`"
        );
        assert_eq!(BridgeErrorCode::UnknownMethod.to_string(), "unknownMethod");
        assert_eq!(BridgeErrorCode::Failed.to_string(), "failed");
    }

    #[test]
    fn window_params_read_the_recorded_shapes() {
        let settings = window_params(include_str!(
            "../../../macos/web/fixtures/bridge/params.window.json"
        ))
        .unwrap();
        assert_eq!(settings.window, BridgeWindow::Settings);
        assert_eq!(settings.section, Some(SettingsSection::Summaries));
        assert!(settings.meeting_id.is_none());

        let main = window_params(
            r#"{"window":"main","meetingID":"00000000-0000-0000-0000-000000000001"}"#,
        )
        .unwrap();
        assert_eq!(main.window, BridgeWindow::Main);
        assert_eq!(
            main.meeting_id.map(|id| uuid_text(&id)).as_deref(),
            Some("00000000-0000-0000-0000-000000000001")
        );
        assert_eq!(
            uuid_text(&Uuid::nil()),
            "00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(
            uuid_text(&"6ba7b810-9dad-11d1-80b4-00c04fd430c8".parse().unwrap()),
            "6BA7B810-9DAD-11D1-80B4-00C04FD430C8"
        );
    }

    #[test]
    fn every_contract_section_parses_and_nothing_else() {
        for section in [
            "general",
            "recording",
            "transcription",
            "summaries",
            "export",
            "iphone",
        ] {
            let params =
                window_params(&format!(r#"{{"window":"settings","section":"{section}"}}"#))
                    .unwrap_or_else(|error| panic!("{section}: {error}"));
            assert_eq!(params.section.unwrap().as_str(), section);
        }
        for json in [
            r#"{"window":"settings","section":"advanced"}"#,
            r#"{"window":"settings","section":"General"}"#,
            r#"{"window":"settings","section":"iphone&dark"}"#,
            r#"{"window":"panel"}"#,
            r#"{"window":"main","extra":true}"#,
        ] {
            let error = window_params(json).expect_err(json);
            assert_eq!(error.code, BridgeErrorCode::InvalidParams, "{json}");
            assert!(error.message.starts_with("window.open: "), "{json}");
        }
    }

    #[test]
    fn a_meeting_id_must_be_a_uuid() {
        for json in [
            r#"{"window":"main","meetingID":"m-1"}"#,
            r#"{"window":"main","meetingID":""}"#,
            r#"{"window":"main","meetingID":"00000000-0000-0000-0000-00000000000g"}"#,
            r#"{"window":"main","meetingID":1}"#,
        ] {
            let error = window_params(json).expect_err(json);
            assert_eq!(error.code, BridgeErrorCode::InvalidParams, "{json}");
        }
    }

    #[test]
    fn only_the_onboarding_window_closes_and_only_itself() {
        let onboarding = window_params(r#"{"window":"onboarding"}"#).unwrap();
        let settings = window_params(r#"{"window":"settings"}"#).unwrap();
        assert_eq!(
            close_target("onboarding", &onboarding).unwrap(),
            BridgeWindow::Onboarding
        );
        let other = close_target("onboarding", &settings).unwrap_err();
        assert_eq!(other.code, BridgeErrorCode::InvalidParams);
        assert_eq!(other.message, "The onboarding window closes only itself.");
        for caller in ["main", "settings"] {
            let error = close_target(caller, &onboarding).unwrap_err();
            assert_eq!(error.code, BridgeErrorCode::UnknownMethod, "{caller}");
            assert_eq!(
                error.message,
                format!("The {caller} window does not answer window.close.")
            );
        }
    }

    #[test]
    fn only_a_finished_onboarding_snapshot_closes_the_window() {
        let setup: Value = serde_json::from_str(include_str!(
            "../../../macos/web/fixtures/bridge/onboarding.setup.json"
        ))
        .unwrap();
        assert!(!finishes_onboarding("onboarding", &setup));
        let mut finished = setup.clone();
        finished["finished"] = Value::Bool(true);
        assert!(finishes_onboarding("onboarding", &finished));
        assert!(!finishes_onboarding("app", &finished));
        assert!(!finishes_onboarding("onboarding", &Value::Null));
    }
}
