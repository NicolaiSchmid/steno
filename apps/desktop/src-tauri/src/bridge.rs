//! The `bridge_call` command: the Tauri side of
//! `apps/macos/web/src/bridge/tauri-transport.ts`. The page sends
//! `{ method, params }` and gets the result or a `BridgeError` with the
//! contract's error codes (`Sources/StenoBridge`, `envelope.error.json`).
//! Window and URL methods are the shell's own; everything else goes to the
//! host.
//!
//! Swift: `BridgeRequestParams.swift`, `WindowRequests.swift`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, EventTarget, Manager, State, Url, WebviewWindow};
use tauri_plugin_opener::OpenerExt;

use crate::{
    host::Host,
    smoke::Smoke,
    windows::{self, BridgeWindow},
};

/// The event every snapshot travels on; the page listens for it scoped to
/// its own window.
pub const EVENT_NAME: &str = "steno:event";

/// The contract's error codes the shell raises, spelled as the page reads
/// them. The full set (`unknownMethod`, `notFound`, `cancelled` as well) is
/// the host's and comes with `steno-bridge`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BridgeErrorCode {
    InvalidParams,
    Failed,
}

/// A rejected command, serialised as the contract's error envelope; the
/// transport turns it into the page's `BridgeError`.
#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{code:?}: {message}")]
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

    pub fn failed(error: impl std::fmt::Display) -> Self {
        Self::new(BridgeErrorCode::Failed, error.to_string())
    }

    pub fn invalid_params(method: &str, error: impl std::fmt::Display) -> Self {
        Self::new(BridgeErrorCode::InvalidParams, format!("{method}: {error}"))
    }
}

impl From<tauri::Error> for BridgeError {
    fn from(error: tauri::Error) -> Self {
        Self::failed(error)
    }
}

/// The event envelope (`envelope.event.json`).
#[derive(Debug, Clone, Serialize)]
struct BridgeEvent<'a> {
    topic: &'a str,
    payload: Value,
}

/// Publishes one topic's snapshot to one window, and to that window only.
/// An `onboarding` snapshot that says `finished` also closes the onboarding
/// window: the page shows what the host says and the host ends the window.
///
/// Swift: `OnboardingWindow.swift` (`onChange(of: finished)`).
pub fn emit(window: &WebviewWindow, topic: &str, payload: Value) -> tauri::Result<()> {
    let finished = finishes_onboarding(topic, &payload);
    window.emit_to(
        EventTarget::webview_window(window.label()),
        EVENT_NAME,
        BridgeEvent { topic, payload },
    )?;
    if finished {
        windows::close(window.app_handle(), BridgeWindow::Onboarding)?;
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
            BridgeError::new(
                BridgeErrorCode::InvalidParams,
                "Only https: and mailto: links open from the page.",
            )
        })
}

/// `params.window.json`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowParams {
    pub window: BridgeWindow,
    pub section: Option<String>,
    #[serde(rename = "meetingID")]
    pub meeting_id: Option<String>,
}

/// `params.system.openURL.json`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenUrlParams {
    url: String,
}

fn parse<T: for<'de> Deserialize<'de>>(method: &str, params: Value) -> Result<T, BridgeError> {
    serde_json::from_value(params).map_err(|error| BridgeError::invalid_params(method, error))
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
            // In the contract for the onboarding window alone; the page does
            // not call it today, the host closes that window on `finished`.
            let request: WindowParams = parse(&method, params)?;
            if request.window != BridgeWindow::Onboarding {
                return Err(BridgeError::invalid_params(
                    &method,
                    "only the onboarding window closes on request",
                ));
            }
            windows::close(&app, request.window)?;
            Ok(Value::Null)
        }
        "system.openURL" => {
            let request: OpenUrlParams = parse(&method, params)?;
            let url = openable_url(&request.url)?;
            app.opener()
                .open_url(url, None::<&str>)
                .map_err(BridgeError::failed)?;
            Ok(Value::Null)
        }
        _ => host.call(&window, &method, params),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFUSAL: &str = "Only https: and mailto: links open from the page.";

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
        let error = BridgeError::invalid_params("window.open", "missing field `window`");
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["code"], "invalidParams");
        assert_eq!(json["message"], "window.open: missing field `window`");
        assert_eq!(
            error.to_string(),
            "InvalidParams: window.open: missing field `window`"
        );
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
