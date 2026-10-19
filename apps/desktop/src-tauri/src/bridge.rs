//! The `bridge_call` command, the Tauri side of the page's transport.
//! Window and URL methods are the shell's own; everything else goes to the
//! host on Tauri's blocking pool ([`host_command`]), since a host call can
//! wait for seconds and would otherwise hold a command worker. Commands
//! stay concurrent there rather than taking the tray's ordered lane.

use std::fmt;

use serde::Deserialize;
use serde_json::Value;
pub use steno_bridge::BridgeError;
use steno_bridge::{
    BridgeEvent, BridgeTopic, BridgeWindow, OpenUrlParams, PermissionKindParams, SetBoolParams,
    WindowParams,
};
use tauri::{AppHandle, Emitter, EventTarget, Manager, State, Url, WebviewWindow};

use crate::{
    actions, autostart, dialogs,
    host::{ShellHost, call_blocking},
    panels::{self, Panel},
    permissions,
    recording::{RecorderState, RecordingState},
    smoke::Smoke,
    tray, windows,
};

/// The event every snapshot travels on, scoped to its window.
pub const EVENT_NAME: &str = "steno:event";

/// A shell-side failure as the contract's `failed`, for `map_err`.
pub fn failed(error: impl fmt::Display) -> BridgeError {
    BridgeError::failed(error.to_string())
}

/// `invalidParams` naming the method whose params did not decode.
fn invalid_params_for(method: &str, error: impl fmt::Display) -> BridgeError {
    BridgeError::invalid_params(format!("{method}: {error}"))
}

/// Publishes one topic's snapshot to one window only. A finished
/// `onboarding` snapshot also closes the onboarding window; a `recording`
/// snapshot to main is what the tray and the panels follow.
pub fn emit(window: &WebviewWindow, topic: &str, payload: Value) -> Result<(), BridgeError> {
    let topic: BridgeTopic = topic.parse().map_err(failed)?;
    let finished = finishes_onboarding(topic, &payload);
    let recording = recording_state_for_shell(window.label(), topic, &payload);
    window
        .state::<Smoke>()
        .note_snapshot(window.label(), topic, &payload);
    window
        .emit_to(
            EventTarget::webview_window(window.label()),
            EVENT_NAME,
            BridgeEvent::new(topic, payload),
        )
        .map_err(failed)?;
    if finished {
        windows::close(window.app_handle(), BridgeWindow::Onboarding).map_err(failed)?;
    }
    if let Some(state) = recording {
        tray::note_recording(window.app_handle(), state);
        panels::note_recording(window.app_handle(), state);
    }
    Ok(())
}

/// The recorder state the shell follows: the `recording` topic as
/// published to the main window; anything else moves nothing.
pub fn recording_state_for_shell(
    label: &str,
    topic: BridgeTopic,
    payload: &Value,
) -> Option<RecordingState> {
    (label == BridgeWindow::Main.as_str() && topic == BridgeTopic::Recording)
        .then(|| RecordingState::from_snapshot(payload))
        .flatten()
}

pub fn finishes_onboarding(topic: BridgeTopic, payload: &Value) -> bool {
    topic == BridgeTopic::Onboarding && payload["finished"] == Value::Bool(true)
}

/// The URLs a page may open: `https:` and `mailto:` only, so a page cannot
/// open files or run scripts; anything else is `invalidParams`.
pub fn openable_url(text: &str) -> Result<Url, BridgeError> {
    Url::parse(text)
        .ok()
        .filter(|url| matches!(url.scheme(), "https" | "mailto"))
        .ok_or_else(|| {
            BridgeError::invalid_params("Only https: and mailto: links open from the page.")
        })
}

steno_core::string_enum! {
    /// What a panel asks of the shell through `panel_call`.
    pub enum PanelAction {
        Resize = "resize",
        DismissPrompt = "dismissPrompt",
        RecordFromPrompt = "recordFromPrompt",
    }
}

/// The action a panel named, or `unknownMethod`.
pub fn panel_action(text: &str) -> Result<PanelAction, BridgeError> {
    text.parse()
        .map_err(|_| BridgeError::unknown_method(format!("The panels do not answer {text}.")))
}

/// `panel_call("resize")`: the page's measured size in device pixels.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResizeParams {
    width: f64,
    height: f64,
}

impl ResizeParams {
    /// The size in logical points for a window of `scale`. CSS pixels are
    /// not points everywhere: `WebKitGTK` takes its pixel ratio from the X
    /// resolution while the window's scale stays 1.
    fn logical(&self, scale: f64) -> (f64, f64) {
        (self.width / scale, self.height / scale)
    }
}

/// `panel_call("dismissPrompt")` and `panel_call("recordFromPrompt")`: the
/// number of the prompt clicked, none for an unnumbered prompt.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptParams {
    raised: Option<u64>,
}

fn parse<T: for<'de> Deserialize<'de>>(method: &str, params: Value) -> Result<T, BridgeError> {
    serde_json::from_value(params).map_err(|error| invalid_params_for(method, error))
}

/// A panel's params: a JSON object (serde would also read a struct from an
/// array), with no unknown field.
fn panel_params<T: for<'de> Deserialize<'de>>(
    action: PanelAction,
    params: Value,
) -> Result<T, BridgeError> {
    if !params.is_object() {
        return Err(invalid_params_for(
            action.as_str(),
            "the params must be an object",
        ));
    }
    parse(action.as_str(), params)
}

/// The X's and Record's params; `null` names whatever shows, as `{}` does.
fn prompt_params(action: PanelAction, params: Value) -> Result<PromptParams, BridgeError> {
    if params.is_null() {
        Ok(PromptParams::default())
    } else {
        panel_params(action, params)
    }
}

/// Which window a `window.close` from `caller` may close: only onboarding,
/// on its own request. Main and Settings answer `unknownMethod`, and
/// onboarding closing another window is `invalidParams`, with the
/// contract's messages.
pub fn close_target(caller: &str, request: &WindowParams) -> Result<BridgeWindow, BridgeError> {
    if caller != BridgeWindow::Onboarding.as_str() {
        // The Settings window is named in title case.
        let name = if caller == BridgeWindow::Settings.as_str() {
            "Settings"
        } else {
            caller
        };
        return Err(BridgeError::unknown_method(format!(
            "The {name} window does not answer window.close."
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
    smoke: State<'_, Smoke>,
    method: String,
    params: Option<Value>,
) -> Result<Value, BridgeError> {
    let params = params.unwrap_or(Value::Null);
    match method.as_str() {
        "page.ready" => {
            smoke.note_ready(window.label());
            host_command(app, window, method, params).await
        }
        "window.open" => {
            let request: WindowParams = parse(&method, params)?;
            windows::open_requested(&app, &request)?;
            Ok(Value::Null)
        }
        "window.close" => {
            let request: WindowParams = parse(&method, params)?;
            let target = close_target(window.label(), &request)?;
            windows::close(&app, target).map_err(failed)?;
            Ok(Value::Null)
        }
        "system.openURL" => {
            let request: OpenUrlParams = parse(&method, params)?;
            let url = openable_url(&request.url)?;
            dialogs::open_url(&app, url.as_str())?;
            Ok(Value::Null)
        }
        // The OS plumbing: settings panes, the login item, the update check
        // and the folder panels.
        "system.openSystemSettings" => {
            let request: PermissionKindParams = parse(&method, params)?;
            if let Some(url) = permissions::system_settings_url(request.kind) {
                dialogs::open_url(&app, &url)?;
            }
            Ok(Value::Null)
        }
        "settings.general.openLoginItems" => {
            if let Some(url) = autostart::system_settings_url() {
                dialogs::open_url(&app, url)?;
            }
            Ok(Value::Null)
        }
        "updates.check" => {
            actions::check_for_updates(&app);
            Ok(Value::Null)
        }
        _ => match dialogs::FolderChooser::for_method(&method) {
            Some(chooser) => {
                let chosen = dialogs::choose_folder(&window, chooser).await?;
                if let Some(path) = &chosen {
                    let reply = dialogs::chosen_path_reply(Some(path));
                    host_command(app, window, method, reply).await?;
                }
                Ok(dialogs::chosen_path_reply(chosen.as_deref()))
            }
            None => host_command(app, window, method, params).await,
        },
    }
}

/// What `bridge_call` hands the host, on Tauri's blocking pool
/// ([`call_blocking`]), so the command worker is free while the host
/// works; permission requests among them wait up to 30 s for the user.
pub async fn host_command<H: ShellHost>(
    host: H,
    window: H::Window,
    method: String,
    params: Value,
) -> Result<Value, BridgeError> {
    call_blocking(host, move |host| match method.as_str() {
        "page.ready" => host.page_ready(&window).map(|()| Value::Null),
        "settings.general.setLaunchAtLogin" => {
            let request: SetBoolParams = parse(&method, params)?;
            actions::set_launch_at_login(host, &window, request.value)?;
            Ok(Value::Null)
        }
        _ => host.call(&window, &method, params),
    })
    .await
}

/// The panels' own command: the page reports its size (`resize`), and the
/// prompt's X dismisses it (`dismissPrompt`) and its Record records the
/// call it announced (`recordFromPrompt`). Bridge windows get
/// `unknownMethod`.
///
/// Synchronous, so Tauri runs it on the main thread in the page's order
/// and the last size reported wins. It must not wait on the main thread or
/// build a window, which would deadlock on Windows.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn panel_call(
    app: AppHandle,
    window: WebviewWindow,
    action: String,
    params: Option<Value>,
) -> Result<Value, BridgeError> {
    let panel = panel_caller(window.label())?;
    let params = params.unwrap_or(Value::Null);
    match panel_action(&action)? {
        PanelAction::Resize => {
            let report: ResizeParams = panel_params(PanelAction::Resize, params)?;
            let size = report.logical(window.scale_factor().map_err(failed)?);
            app.state::<Smoke>().note_panel_size(panel, size);
            panels::resize(&app, panel, size)?;
            Ok(Value::Null)
        }
        PanelAction::DismissPrompt => {
            let request = prompt_params(PanelAction::DismissPrompt, params)?;
            panels::dismiss_prompt(&app, request.raised);
            Ok(Value::Null)
        }
        PanelAction::RecordFromPrompt => {
            let request = prompt_params(PanelAction::RecordFromPrompt, params)?;
            panels::record_from_prompt(&app, request.raised);
            Ok(Value::Null)
        }
    }
}

/// Which panel is calling, or `unknownMethod` for any other window.
pub fn panel_caller(label: &str) -> Result<Panel, BridgeError> {
    Panel::from_label(label).ok_or_else(|| {
        BridgeError::unknown_method(format!("The {label} window does not answer panel_call."))
    })
}

#[cfg(test)]
mod tests {
    use ::uuid::Uuid;
    use steno_bridge::{BridgeErrorCode, SettingsSection};
    use steno_core::json::uuid_string;

    use super::*;

    const REFUSAL: &str = "Only https: and mailto: links open from the page.";

    fn window_params(json: &str) -> Result<WindowParams, BridgeError> {
        parse("window.open", serde_json::from_str(json).unwrap())
    }

    /// A held host call does not block another command on a one-worker
    /// runtime: it waits on the blocking pool.
    #[test]
    fn a_held_host_call_leaves_the_command_workers_free() {
        use std::time::Duration;

        use crate::host::gated::GatedHost;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let host = GatedHost::holding("recording.toggle");
        let _opens = host.opens_on_drop();
        let (sender, answers) = std::sync::mpsc::channel();
        let command = |method: &'static str, params: Value| {
            let (host, sender) = (host.clone(), sender.clone());
            runtime.spawn(async move {
                let answer = host_command(host, "main", method.to_owned(), params).await;
                sender.send((method, answer)).unwrap();
            });
        };
        command("recording.toggle", Value::Null);
        assert!(
            host.wait_held(Duration::from_secs(5)),
            "the Stop reached the host"
        );
        command("meetings.select", serde_json::json!({ "meetingId": "m-1" }));
        command(
            "settings.general.setLaunchAtLogin",
            serde_json::json!({ "value": true }),
        );
        let mut answered: Vec<_> = (0..2)
            .map(|_| answers.recv_timeout(Duration::from_secs(1)).unwrap())
            .collect();
        answered.sort_by_key(|(method, _)| *method);
        assert_eq!(
            answered,
            [
                ("meetings.select", Ok(Value::from("meetings.select"))),
                ("settings.general.setLaunchAtLogin", Ok(Value::Null)),
            ],
            "the other commands answer while the Stop is held"
        );
        assert_eq!(host.switched(), [true]);
        assert!(answers.try_recv().is_err(), "the Stop is still held");
        host.open();
        assert_eq!(
            answers.recv_timeout(Duration::from_secs(5)).unwrap(),
            ("recording.toggle", Ok(Value::from("recording.toggle")))
        );
        runtime.shutdown_background();
    }

    /// Each permission request runs on the blocking pool, where blocking
    /// on another runtime does not panic.
    #[test]
    fn a_permission_request_leaves_the_async_worker() {
        use crate::host::gated::GatedHost;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        for method in [
            "onboarding.request",
            "settings.recording.requestPermission",
            "settings.general.requestCalendar",
        ] {
            assert!(
                method.parse::<steno_bridge::BridgeMethod>().is_ok(),
                "{method} is a bridge method"
            );
            let other = std::sync::Arc::new(
                tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap(),
            );
            let blocks = std::sync::Arc::clone(&other);
            let host = GatedHost::holding(method).before_hold(move || blocks.block_on(async {}));
            host.open();
            let answer = runtime.block_on(runtime.spawn(host_command(
                host.clone(),
                "main",
                method.to_owned(),
                Value::Null,
            )));
            assert_eq!(answer.unwrap(), Ok(Value::from(method)), "{method}");
            drop(host);
            drop(other);
        }
        runtime.shutdown_background();
    }

    /// Only https: and mailto: open, the scheme in any case; every other
    /// scheme and an unparsable URL are invalid params.
    #[test]
    fn only_https_and_mailto_links_open() {
        let github = openable_url("https://github.com/NicolaiSchmid/steno").unwrap();
        assert_eq!(github.host_str(), Some("github.com"));
        let mail = openable_url("mailto:hello@example.com").unwrap();
        assert_eq!(mail.path(), "hello@example.com");
        assert!(openable_url("HTTPS://example.com/").is_ok());
        for text in [
            "http://example.com/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "steno://meeting/1",
            "",
            "not a url",
            "example.com/no-scheme",
            "https://",
        ] {
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
            main.meeting_id.map(uuid_string).as_deref(),
            Some("00000000-0000-0000-0000-000000000001")
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
        ] {
            let error = window_params(json).expect_err(json);
            assert_eq!(error.code, BridgeErrorCode::InvalidParams, "{json}");
            assert!(error.message.starts_with("window.open: "), "{json}");
        }
        // Unknown keys are ignored.
        let params = window_params(r#"{"window":"main","extra":true}"#).unwrap();
        assert_eq!(params.window, BridgeWindow::Main);
    }

    /// Either case of the hyphenated UUID form; `null` and a missing key
    /// are no meeting.
    #[test]
    fn a_meeting_id_must_be_a_hyphenated_uuid() {
        let id: Uuid = "00000000-0000-0000-0000-00000000000c".parse().unwrap();
        for json in [
            r#"{"window":"main","meetingID":"00000000-0000-0000-0000-00000000000c"}"#,
            r#"{"window":"main","meetingID":"00000000-0000-0000-0000-00000000000C"}"#,
        ] {
            assert_eq!(window_params(json).unwrap().meeting_id, Some(id), "{json}");
        }
        for json in [
            r#"{"window":"main"}"#,
            r#"{"window":"main","meetingID":null}"#,
        ] {
            assert_eq!(window_params(json).unwrap().meeting_id, None, "{json}");
        }
        for json in [
            r#"{"window":"main","meetingID":"m-1"}"#,
            r#"{"window":"main","meetingID":""}"#,
            r#"{"window":"main","meetingID":"00000000-0000-0000-0000-00000000000g"}"#,
            r#"{"window":"main","meetingID":"00000000-0000-0000-0000-00000000000"}"#,
            r#"{"window":"main","meetingID":"0000000000000000000000000000000c"}"#,
            r#"{"window":"main","meetingID":"{00000000-0000-0000-0000-00000000000c}"}"#,
            r#"{"window":"main","meetingID":"urn:uuid:00000000-0000-0000-0000-00000000000c"}"#,
            r#"{"window":"main","meetingID":1}"#,
        ] {
            let error = window_params(json).expect_err(json);
            assert_eq!(error.code, BridgeErrorCode::InvalidParams, "{json}");
        }
        let braced = window_params(
            r#"{"window":"main","meetingID":"{00000000-0000-0000-0000-00000000000c}"}"#,
        )
        .unwrap_err();
        assert!(
            braced.message.starts_with("window.open: not a UUID: {0000"),
            "{}",
            braced.message
        );
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
        for (caller, name) in [("main", "main"), ("settings", "Settings")] {
            let error = close_target(caller, &onboarding).unwrap_err();
            assert_eq!(error.code, BridgeErrorCode::UnknownMethod, "{caller}");
            assert_eq!(
                error.message,
                format!("The {name} window does not answer window.close.")
            );
        }
    }

    #[test]
    fn only_the_main_windows_recording_snapshot_moves_the_shell() {
        let live: Value = serde_json::from_str(include_str!(
            "../../../macos/web/fixtures/bridge/recording.live.json"
        ))
        .unwrap();
        assert_eq!(
            recording_state_for_shell("main", BridgeTopic::Recording, &live),
            Some(RecordingState::Recording)
        );
        assert_eq!(
            recording_state_for_shell("settings", BridgeTopic::Recording, &live),
            None
        );
        assert_eq!(
            recording_state_for_shell("main", BridgeTopic::Progress, &live),
            None
        );
        assert_eq!(
            recording_state_for_shell("main", BridgeTopic::Recording, &Value::Null),
            None
        );
    }

    #[test]
    fn only_the_panels_call_panel_call() {
        assert_eq!(panel_caller("bubble").unwrap(), Panel::Bubble);
        assert_eq!(panel_caller("prompt").unwrap(), Panel::Prompt);
        for label in ["main", "settings", "onboarding"] {
            let error = panel_caller(label).unwrap_err();
            assert_eq!(error.code, BridgeErrorCode::UnknownMethod, "{label}");
            assert_eq!(
                error.message,
                format!("The {label} window does not answer panel_call.")
            );
        }
    }

    #[test]
    fn the_shells_params_read_the_recorded_shapes() {
        let flag: SetBoolParams = parse(
            "settings.general.setLaunchAtLogin",
            serde_json::from_str(include_str!(
                "../../../macos/web/fixtures/bridge/params.bool.json"
            ))
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(flag.value, true | false));
        let size: ResizeParams = parse(
            "resize",
            serde_json::json!({ "width": 244.5, "height": 40 }),
        )
        .unwrap();
        assert_eq!((size.width, size.height), (244.5, 40.0));
        assert_eq!(size.logical(1.0), (244.5, 40.0));
        // A 1.25 ratio on a window of scale 1, and a Retina window.
        let large: ResizeParams = parse(
            "resize",
            serde_json::json!({ "width": 100.0, "height": 52.5 }),
        )
        .unwrap();
        assert_eq!(large.logical(1.0), (100.0, 52.5));
        assert_eq!(large.logical(2.0), (50.0, 26.25));
        for (params, says) in [
            (serde_json::json!({ "width": 1 }), "missing field `height`"),
            (serde_json::json!([300, 40]), "the params must be an object"),
            (
                serde_json::json!({ "width": 300, "height": 40, "depth": 2 }),
                "unknown field `depth`",
            ),
            (Value::Null, "the params must be an object"),
        ] {
            let error = panel_params::<ResizeParams>(PanelAction::Resize, params).unwrap_err();
            assert_eq!(error.code, BridgeErrorCode::InvalidParams);
            assert!(error.message.starts_with("resize: "), "{}", error.message);
            assert!(error.message.contains(says), "{}", error.message);
        }
    }

    #[test]
    fn a_dismissal_or_a_record_names_its_prompt_or_none() {
        for action in [PanelAction::DismissPrompt, PanelAction::RecordFromPrompt] {
            let named: PromptParams =
                panel_params(action, serde_json::json!({ "raised": 3 })).unwrap();
            assert_eq!(named.raised, Some(3));
            let none: PromptParams = panel_params(action, serde_json::json!({})).unwrap();
            assert_eq!(none.raised, None);
            // No params at all counts too; `panel_params` alone refuses it.
            assert_eq!(prompt_params(action, Value::Null).unwrap().raised, None);
            assert_eq!(
                prompt_params(action, serde_json::json!({ "raised": 3 }))
                    .unwrap()
                    .raised,
                Some(3)
            );
            for params in [
                serde_json::json!({ "raised": -1 }),
                serde_json::json!({ "raised": "3" }),
                serde_json::json!({ "raised": 3, "app": "Zoom" }),
                serde_json::json!([3]),
            ] {
                let error = prompt_params(action, params.clone()).unwrap_err();
                assert_eq!(error.code, BridgeErrorCode::InvalidParams, "{params}");
                assert!(
                    error.message.starts_with(&format!("{}: ", action.as_str())),
                    "{params}"
                );
            }
        }
    }

    #[test]
    fn the_panels_answer_three_actions_and_nothing_else() {
        assert_eq!(panel_action("resize").unwrap(), PanelAction::Resize);
        assert_eq!(
            panel_action("dismissPrompt").unwrap(),
            PanelAction::DismissPrompt
        );
        assert_eq!(
            panel_action("recordFromPrompt").unwrap(),
            PanelAction::RecordFromPrompt
        );
        for text in ["Resize", "close", ""] {
            let error = panel_action(text).unwrap_err();
            assert_eq!(error.code, BridgeErrorCode::UnknownMethod, "{text}");
            assert_eq!(error.message, format!("The panels do not answer {text}."));
        }
    }

    #[test]
    fn only_a_finished_onboarding_snapshot_closes_the_window() {
        let setup: Value = serde_json::from_str(include_str!(
            "../../../macos/web/fixtures/bridge/onboarding.setup.json"
        ))
        .unwrap();
        assert!(!finishes_onboarding(BridgeTopic::Onboarding, &setup));
        let mut finished = setup.clone();
        finished["finished"] = Value::Bool(true);
        assert!(finishes_onboarding(BridgeTopic::Onboarding, &finished));
        assert!(!finishes_onboarding(BridgeTopic::App, &finished));
        assert!(!finishes_onboarding(BridgeTopic::Onboarding, &Value::Null));
    }

    /// A panic in a host call resumes in the awaiting command rather than
    /// turning into a `failed` answer.
    #[test]
    fn a_panic_in_a_host_call_resumes_in_the_awaiting_command() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(crate::host::call_blocking(
                (),
                |(): &()| -> Result<(), BridgeError> { panic!("the host panicked") },
            ))
        }));
        let payload = outcome.expect_err("the panic resumes in the awaiting command");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"the host panicked"));
    }
}
