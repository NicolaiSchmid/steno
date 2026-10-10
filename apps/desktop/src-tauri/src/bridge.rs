//! The `bridge_call` command: the Tauri side of
//! `apps/macos/web/src/bridge/tauri-transport.ts`. The page sends
//! `{ method, params }` and gets the result or a `BridgeError` with the
//! contract's error codes (`Sources/StenoBridge`, `envelope.error.json`).
//! Window and URL methods are the shell's own; everything else goes to the
//! host.
//!
//! Swift: `BridgeRequestParams.swift`, `WindowRequests.swift`.

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
    host::Host,
    panels::{self, Panel},
    permissions,
    recording::{RecorderState, RecordingState},
    smoke::Smoke,
    tray, windows,
};

/// The event every snapshot travels on; the page listens for it scoped to
/// its own window.
pub const EVENT_NAME: &str = "steno:event";

/// A shell-side failure (a window call, the opener, a plugin) as the
/// contract's `failed`, for `map_err`.
pub fn failed(error: impl fmt::Display) -> BridgeError {
    BridgeError::failed(error.to_string())
}

/// `invalidParams` for a method whose params did not decode, named after
/// the method so the page's log says which.
fn invalid_params_for(method: &str, error: impl fmt::Display) -> BridgeError {
    BridgeError::invalid_params(format!("{method}: {error}"))
}

/// Publishes one topic's snapshot to one window, and to that window only;
/// the smoke run counts what reaches main. An `onboarding` snapshot that
/// says `finished` also closes the onboarding window: the page shows what
/// the host says and the host ends the window. A `recording` snapshot to
/// the main window is also what the tray and the floating panels follow.
///
/// Swift: `OnboardingWindow.swift` (`onChange(of: finished)`),
/// `FloatingPanelPresenter.observe` and `MenuBarLabel`.
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
        #[cfg(target_os = "linux")]
        crate::session_end::note_recording(window.app_handle(), state);
    }
    Ok(())
}

/// The recorder state the shell follows: the `recording` topic as published
/// to the main window (the window the host drives the recorder through);
/// the same topic reaching another window, or any other topic, moves
/// nothing.
pub fn recording_state_for_shell(
    label: &str,
    topic: BridgeTopic,
    payload: &Value,
) -> Option<RecordingState> {
    (label == BridgeWindow::Main.as_str() && topic == BridgeTopic::Recording)
        .then(|| RecordingState::from_snapshot(payload))
        .flatten()
}

/// Whether a snapshot ends onboarding: the `onboarding` topic with
/// `finished` true.
pub fn finishes_onboarding(topic: BridgeTopic, payload: &Value) -> bool {
    topic == BridgeTopic::Onboarding && payload["finished"] == Value::Bool(true)
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

steno_core::string_enum! {
    /// What a panel asks of the shell through `panel_call` (`PanelAction`
    /// in `panel-shell.ts`).
    pub enum PanelAction {
        Resize = "resize",
        DismissPrompt = "dismissPrompt",
        RecordFromPrompt = "recordFromPrompt",
    }
}

/// The action a panel named, or `unknownMethod`, as the bridge answers a
/// method it does not know.
pub fn panel_action(text: &str) -> Result<PanelAction, BridgeError> {
    text.parse()
        .map_err(|_| BridgeError::unknown_method(format!("The panels do not answer {text}.")))
}

/// `panel_call("resize")`: the page's measured size in device pixels (CSS
/// pixels times `devicePixelRatio`, `deviceSize` in `panel-shell.ts`).
/// Exactly the two fields, as an object.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResizeParams {
    width: f64,
    height: f64,
}

impl ResizeParams {
    /// The size in logical points for a window of `scale`. CSS pixels are
    /// not points everywhere: `WebKitGTK` takes its pixel ratio from the X
    /// resolution (1.25 at 120 dpi) while the window's scale stays 1.
    fn logical(&self, scale: f64) -> (f64, f64) {
        (self.width / scale, self.height / scale)
    }
}

/// `panel_call("dismissPrompt")` and `panel_call("recordFromPrompt")`:
/// the number of the prompt whose X or Record was clicked (`raised` in
/// its route); `null` params or no `raised` for a prompt shown
/// unnumbered.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptParams {
    raised: Option<u64>,
}

fn parse<T: for<'de> Deserialize<'de>>(method: &str, params: Value) -> Result<T, BridgeError> {
    serde_json::from_value(params).map_err(|error| invalid_params_for(method, error))
}

/// A panel's params: a JSON object (serde would also read a struct from
/// an array, `[300, 40]`), with no field the action does not name.
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

/// The X's and Record's params: none (`null`) names whatever shows, as
/// `{}` does.
fn prompt_params(action: PanelAction, params: Value) -> Result<PromptParams, BridgeError> {
    if params.is_null() {
        Ok(PromptParams::default())
    } else {
        panel_params(action, params)
    }
}

/// Which window a `window.close` from `caller` may close: the onboarding
/// window, and only on its own request. The main and Settings windows do
/// not answer the method (`unknownMethod`, as their Swift hosts route it,
/// `MainWindowBridge.swift` and `SettingsBridge.swift`); the onboarding
/// window closes nothing but itself (`invalidParams`,
/// `OnboardingBridge.swift`). The messages are the Swift hosts' verbatim.
/// The page does not send the method today: the host closes the window on
/// a finished `onboarding` snapshot (`emit`).
pub fn close_target(caller: &str, request: &WindowParams) -> Result<BridgeWindow, BridgeError> {
    if caller != BridgeWindow::Onboarding.as_str() {
        // The Swift hosts name the Settings window with its title case.
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
            // What was asked of the window before its page could hear it
            // (a deep link at launch) goes out after the first snapshots.
            if let Some(owed) = app.state::<windows::Pages>().ready(window.label()) {
                host.publish_request(&window, owed.field.as_str(), &owed.value)?;
            }
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
            windows::close(&app, target).map_err(failed)?;
            Ok(Value::Null)
        }
        "system.openURL" => {
            let request: OpenUrlParams = parse(&method, params)?;
            let url = openable_url(&request.url)?;
            dialogs::open_url(&app, url.as_str())?;
            Ok(Value::Null)
        }
        // The OS plumbing the Swift host did inside its view models and
        // the shell does here: settings panes, the login item, the update
        // check and the folder panels. See each module for the seam
        // towards the host.
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
        // The host hears of it too, so its General snapshot follows.
        "settings.general.setLaunchAtLogin" => {
            let request: SetBoolParams = parse(&method, params)?;
            actions::set_launch_at_login(&app, &window, request.value)?;
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
                    host.call(&window, &method, dialogs::chosen_path_reply(Some(path)))?;
                }
                Ok(dialogs::chosen_path_reply(chosen.as_deref()))
            }
            None => host.call(&window, &method, params),
        },
    }
}

/// The panels' own command, beside the bridge: the page reports its
/// measured size (`resize`), the prompt's X dismisses the prompt
/// (`dismissPrompt`) and its Record records the call it announced
/// (`recordFromPrompt`, which the detection controller attributes to the
/// prompt's app, as no bridge method could). Only a panel window may call
/// it; the three bridge
/// windows get `unknownMethod`, as they would for a method they do not
/// answer.
///
/// A synchronous command: Tauri runs it on the main thread, in the order
/// the page sent its calls, so a burst of size reports applies in order
/// and the last one sent is the size the window keeps. Nothing in it waits
/// on the main thread (there the window getters answer at once), and it
/// builds no window, which would deadlock a synchronous command on
/// Windows: a dismissal's refresh runs later (`panels::dismiss_prompt`).
// Tauri hands a command its arguments by value.
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
        // `Display` spells the code as the wire does.
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
        // Unknown keys are ignored, as the Swift `Decodable` ignores them.
        let params = window_params(r#"{"window":"main","extra":true}"#).unwrap();
        assert_eq!(params.window, BridgeWindow::Main);
    }

    /// `UUID(uuidString:)` reads either case of the hyphenated form and
    /// nothing else; `null` and a missing key are no meeting.
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
        // Verbatim from `MainWindowBridge.swift` and `SettingsBridge.swift`.
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
        // Device pixels at a ratio of 1.25 (120 dpi) on a window of scale 1,
        // and on a Retina window, where the page's ratio is the scale.
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
            // A click with no params at all (the page sends none for a
            // prompt shown unnumbered) counts too; `panel_params` alone
            // refuses it.
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
}
