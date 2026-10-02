//! The `bridge_call` command: the Tauri side of
//! `apps/macos/web/src/bridge/tauri-transport.ts`. The page sends
//! `{ method, params }` and gets the result or a `BridgeFailure` with the
//! contract's error codes (`Sources/StenoBridge`, `envelope.error.json`).
//! Window and URL methods are the shell's own; everything else goes to the
//! host.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, EventTarget, State, WebviewWindow};
use tauri_plugin_opener::OpenerExt;

use crate::{
    host::Host,
    smoke,
    windows::{self, Kind},
};

/// The event every snapshot travels on; the page listens for it scoped to
/// its own window.
pub const EVENT_NAME: &str = "steno:event";

/// The contract's error codes, spelled as the page reads them. The shell
/// raises `invalidParams` and `failed`; the rest are the host's (WP6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub enum ErrorCode {
    UnknownMethod,
    InvalidParams,
    NotFound,
    Failed,
    Cancelled,
}

/// A rejected command; the transport turns it into a `BridgeError`.
#[derive(Debug, Clone, Serialize)]
pub struct BridgeFailure {
    pub code: ErrorCode,
    pub message: String,
}

impl BridgeFailure {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn failed(error: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::Failed, error.to_string())
    }

    pub fn invalid_params(method: &str, error: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::InvalidParams, format!("{method}: {error}"))
    }
}

impl From<tauri::Error> for BridgeFailure {
    fn from(error: tauri::Error) -> Self {
        Self::failed(error)
    }
}

/// The event envelope (`envelope.event.json`).
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(not(feature = "fixture-host"), allow(dead_code))]
struct BridgeEvent<'a> {
    topic: &'a str,
    payload: Value,
}

/// Publishes one topic's snapshot to one window, and to that window only.
#[cfg_attr(not(feature = "fixture-host"), allow(dead_code))]
pub fn emit(window: &WebviewWindow, topic: &str, payload: Value) -> tauri::Result<()> {
    window.emit_to(
        EventTarget::webview_window(window.label()),
        EVENT_NAME,
        BridgeEvent { topic, payload },
    )
}

/// `params.window.json`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowParams {
    pub window: Kind,
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

fn parse<T: for<'de> Deserialize<'de>>(method: &str, params: Value) -> Result<T, BridgeFailure> {
    serde_json::from_value(params).map_err(|error| BridgeFailure::invalid_params(method, error))
}

#[tauri::command]
pub async fn bridge_call(
    app: AppHandle,
    window: WebviewWindow,
    host: State<'_, Host>,
    method: String,
    params: Option<Value>,
) -> Result<Value, BridgeFailure> {
    let params = params.unwrap_or(Value::Null);
    match method.as_str() {
        "page.ready" => {
            eprintln!("[steno-desktop] page.ready from {}", window.label());
            smoke::note_ready(window.label());
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
            if request.window != Kind::Onboarding {
                return Err(BridgeFailure::invalid_params(
                    &method,
                    "only the onboarding window closes on request",
                ));
            }
            windows::close(&app, request.window)?;
            Ok(Value::Null)
        }
        "system.openURL" => {
            let request: OpenUrlParams = parse(&method, params)?;
            app.opener()
                .open_url(request.url, None::<&str>)
                .map_err(BridgeFailure::failed)?;
            Ok(Value::Null)
        }
        _ => host.call(&window, &method, params),
    }
}
