//! The optional fields no fixture sets. The Swift fixtures omit them, so the
//! round-trip test cannot pin their key or shape; these tests do, one per
//! owning struct, with `contract.ts` as the reference for the shape: a
//! `.optional()` string, number, uuid or enum that appears under its camelCase
//! key when set and is absent when `None`.

mod common;

use chrono::{TimeZone, Utc};
use common::bridge_fixture as fixture;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use steno_bridge::*;
use uuid::Uuid;

/// Encodes, checks the value decodes back to itself, and returns the JSON.
fn encoded<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) -> Value {
    let json = serde_json::to_value(value).unwrap();
    let again: T = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(&again, value, "the value does not survive a round trip");
    json
}

fn uuid(last: u8) -> Uuid {
    Uuid::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, last])
}

#[test]
fn app_snapshot_deep_link_fields() {
    let mut app: AppSnapshot = fixture("app");
    let before = encoded(&app);
    assert!(before.get("requestedMeetingID").is_none());
    assert!(before.get("requestedSettingsSection").is_none());

    app.requested_meeting_id = Some(uuid(0x2a));
    app.requested_settings_section = Some(SettingsSection::Phone);
    let json = encoded(&app);
    assert_eq!(
        json["requestedMeetingID"],
        "00000000-0000-0000-0000-00000000002A"
    );
    assert_eq!(json["requestedSettingsSection"], "iphone");
}

#[test]
fn recording_snapshot_warning_and_error() {
    let mut recording: RecordingSnapshot = fixture("recording.live");
    let before = encoded(&recording);
    assert!(before.get("warning").is_none());
    assert!(before.get("error").is_none());

    recording.warning = Some("The microphone went quiet.".into());
    recording.error = Some("System audio stopped.".into());
    let json = encoded(&recording);
    assert_eq!(json["warning"], "The microphone went quiet.");
    assert_eq!(json["error"], "System audio stopped.");
}

#[test]
fn meetings_list_snapshot_tag_filter_and_error() {
    let mut list: MeetingsListSnapshot = fixture("meetings.list");
    let before = encoded(&list);
    assert!(before.get("tagFilter").is_none());
    assert!(before.get("error").is_none());

    list.tag_filter = Some("hiring".into());
    list.error = Some("The database is locked.".into());
    let json = encoded(&list);
    assert_eq!(json["tagFilter"], "hiring");
    assert_eq!(json["error"], "The database is locked.");
}

#[test]
fn meeting_detail_snapshot_end_reason_and_error() {
    let mut detail: MeetingDetailSnapshot = fixture("meeting.detail");
    let before = encoded(&detail);
    assert!(before.get("endReason").is_none());
    assert!(before.get("error").is_none());

    detail.end_reason = Some("Stopped after 20 minutes of silence.".into());
    detail.error = Some("The notes could not be saved.".into());
    let json = encoded(&detail);
    assert_eq!(json["endReason"], "Stopped after 20 minutes of silence.");
    assert_eq!(json["error"], "The notes could not be saved.");
}

#[test]
fn detail_summary_status_texts() {
    let status = DetailSummaryStatus {
        kind: DetailSummaryStatusKind::SkippedUnconfigured,
        title: Some("No summary".into()),
        body: Some("Set up summaries to get one.".into()),
        action_title: Some("Set up".into()),
    };
    assert_eq!(
        encoded(&status),
        json!({
            "kind": "skippedUnconfigured",
            "title": "No summary",
            "body": "Set up summaries to get one.",
            "actionTitle": "Set up",
        })
    );
    let bare = DetailSummaryStatus {
        kind: DetailSummaryStatusKind::Present,
        title: None,
        body: None,
        action_title: None,
    };
    assert_eq!(encoded(&bare), json!({"kind": "present"}));
}

#[test]
fn onboarding_vault_error_details() {
    let vault = OnboardingVault {
        path: Some("/Users/nicolai/Notes".into()),
        name: Some("Notes".into()),
        validation_message: Some("Pick a folder inside your vault.".into()),
        error: Some("The vault could not be saved.".into()),
        error_details: Some("EACCES: permission denied".into()),
    };
    assert_eq!(
        encoded(&vault),
        json!({
            "path": "/Users/nicolai/Notes",
            "name": "Notes",
            "validationMessage": "Pick a folder inside your vault.",
            "error": "The vault could not be saved.",
            "errorDetails": "EACCES: permission denied",
        })
    );
    assert_eq!(encoded(&OnboardingVault::default()), json!({}));
}

#[test]
fn general_settings_snapshot_error_details() {
    let mut general: GeneralSettingsSnapshot = fixture("settings.general");
    assert!(encoded(&general).get("errorDetails").is_none());

    general.error = Some("Could not read login item state.".into());
    general.error_details = Some("SMAppService returned 1".into());
    let json = encoded(&general);
    assert_eq!(json["error"], "Could not read login item state.");
    assert_eq!(json["errorDetails"], "SMAppService returned 1");
}

#[test]
fn general_updates_last_check_and_detail() {
    let updates = GeneralUpdates {
        can_check: true,
        automatically_checks: true,
        automatically_downloads: false,
        last_check_at: Some(Utc.with_ymd_and_hms(2026, 9, 29, 12, 48, 0).unwrap()),
        outcome: GeneralUpdatesOutcome::Failed,
        detail: Some("The update server did not answer.".into()),
    };
    assert_eq!(
        encoded(&updates),
        json!({
            "canCheck": true,
            "automaticallyChecks": true,
            "automaticallyDownloads": false,
            "lastCheckAt": "2026-09-29T12:48:00.000Z",
            "outcome": "failed",
            "detail": "The update server did not answer.",
        })
    );
}

#[test]
fn recording_settings_snapshot_input_device_and_error_details() {
    let mut recording: RecordingSettingsSnapshot = fixture("settings.recording");
    let before = encoded(&recording);
    assert!(before.get("inputDeviceUID").is_none());
    assert!(before.get("errorDetails").is_none());

    recording.input_device_uid = Some("BuiltInMicrophoneDevice".into());
    recording.error_details = Some("CoreAudio -10851".into());
    let json = encoded(&recording);
    assert_eq!(json["inputDeviceUID"], "BuiltInMicrophoneDevice");
    assert_eq!(json["errorDetails"], "CoreAudio -10851");
}

#[test]
fn transcription_settings_snapshot_error_details() {
    let mut transcription: TranscriptionSettingsSnapshot = fixture("settings.transcription");
    assert!(encoded(&transcription).get("errorDetails").is_none());

    transcription.error_details = Some("The model store is read only.".into());
    assert_eq!(
        encoded(&transcription)["errorDetails"],
        "The model store is read only."
    );
}

#[test]
fn transcription_asset_failure() {
    let asset = TranscriptionAsset {
        id: "parakeet-tdt-0.6b-v3".into(),
        name: "Parakeet".into(),
        detail: "600 MB".into(),
        state: TranscriptionAssetState::Failed,
        download_fraction: None,
        download_phase: None,
        installed_bytes: None,
        failure: Some("The download was interrupted.".into()),
    };
    assert_eq!(
        encoded(&asset),
        json!({
            "id": "parakeet-tdt-0.6b-v3",
            "name": "Parakeet",
            "detail": "600 MB",
            "state": "failed",
            "failure": "The download was interrupted.",
        })
    );
}

#[test]
fn summaries_settings_snapshot_validation_message_and_error_details() {
    let mut summaries: SummariesSettingsSnapshot = fixture("settings.summaries");
    let before = encoded(&summaries);
    assert!(before.get("validationMessage").is_none());
    assert!(before.get("errorDetails").is_none());

    summaries.validation_message = Some("Enter a model name.".into());
    summaries.error_details = Some("401 Unauthorized".into());
    let json = encoded(&summaries);
    assert_eq!(json["validationMessage"], "Enter a model name.");
    assert_eq!(json["errorDetails"], "401 Unauthorized");
}

#[test]
fn summaries_codex_models_error() {
    let codex = SummariesCodex {
        confirmed: true,
        sign_in: SummariesCodexSignIn::SignedIn,
        sign_in_detail: Some("nicolai@example.com".into()),
        model: String::new(),
        models: vec![],
        is_loading_models: false,
        models_error: Some("The model list could not be loaded.".into()),
    };
    assert_eq!(
        encoded(&codex),
        json!({
            "confirmed": true,
            "signIn": "signedIn",
            "signInDetail": "nicolai@example.com",
            "model": "",
            "models": [],
            "isLoadingModels": false,
            "modelsError": "The model list could not be loaded.",
        })
    );
}

#[test]
fn export_settings_snapshot_vault_validation_and_error_details() {
    let mut export: ExportSettingsSnapshot = fixture("settings.export");
    let before = encoded(&export);
    for key in [
        "vaultPath",
        "vaultName",
        "validationMessage",
        "errorDetails",
    ] {
        assert!(before.get(key).is_none(), "{key} is set in the fixture");
    }

    export.vault_path = Some("/Users/nicolai/Notes".into());
    export.vault_name = Some("Notes".into());
    export.validation_message = Some("The people folder must be inside the vault.".into());
    export.error_details = Some("EACCES: permission denied".into());
    let json = encoded(&export);
    assert_eq!(json["vaultPath"], "/Users/nicolai/Notes");
    assert_eq!(json["vaultName"], "Notes");
    assert_eq!(
        json["validationMessage"],
        "The people folder must be inside the vault."
    );
    assert_eq!(json["errorDetails"], "EACCES: permission denied");
}

#[test]
fn phone_settings_snapshot_error_details() {
    let mut phone: PhoneSettingsSnapshot = fixture("settings.iphone");
    assert!(encoded(&phone).get("errorDetails").is_none());

    phone.error_details = Some("Bonjour registration failed (-72000)".into());
    assert_eq!(
        encoded(&phone)["errorDetails"],
        "Bonjour registration failed (-72000)"
    );
}

#[test]
fn phone_listener_failure() {
    let listener = PhoneListener {
        state: PhoneListenerState::Failed,
        port: None,
        failure: Some("Port 7345 is in use.".into()),
    };
    assert_eq!(
        encoded(&listener),
        json!({"state": "failed", "failure": "Port 7345 is in use."})
    );
    let listening = PhoneListener {
        state: PhoneListenerState::Listening,
        port: Some(7345),
        failure: None,
    };
    assert_eq!(
        encoded(&listening),
        json!({"state": "listening", "port": 7345})
    );
}

#[test]
fn bridge_reply_result_carries_any_json_value() {
    let reply: BridgeReply = fixture("envelope.reply");
    assert!(reply.result.is_none());

    let with_result = BridgeReply::result(reply.id.clone(), json!({"confirmed": true}));
    assert_eq!(
        encoded(&with_result),
        json!({"id": "req-1", "result": {"confirmed": true}})
    );
    let dated = BridgeReply::result(
        "req-2",
        serde_json::to_value(PhonePairing {
            expires_at: Utc.with_ymd_and_hms(2026, 9, 29, 12, 48, 0).unwrap(),
            qr_png_base64: "iVBORw0KGgo=".into(),
        })
        .unwrap(),
    );
    assert_eq!(
        encoded(&dated),
        json!({
            "id": "req-2",
            "result": {"expiresAt": "2026-09-29T12:48:00.000Z", "qrPNGBase64": "iVBORw0KGgo="},
        })
    );
}

#[test]
fn window_params_section_and_meeting_id() {
    let params = WindowParams {
        window: BridgeWindow::Main,
        section: Some(SettingsSection::Export),
        meeting_id: Some(uuid(0x2a)),
    };
    assert_eq!(
        encoded(&params),
        json!({
            "window": "main",
            "section": "export",
            "meetingID": "00000000-0000-0000-0000-00000000002A",
        })
    );
    let bare = WindowParams {
        window: BridgeWindow::Settings,
        section: None,
        meeting_id: None,
    };
    assert_eq!(encoded(&bare), json!({"window": "settings"}));
}

#[test]
fn summaries_update_params_every_field() {
    let params = SummariesUpdateParams {
        base_url: Some("https://api.openai.com/v1".into()),
        model: Some("gpt-5".into()),
        context_tokens: Some("128000".into()),
        api_key: Some("sk-test".into()),
    };
    assert_eq!(
        encoded(&params),
        json!({
            "baseURL": "https://api.openai.com/v1",
            "model": "gpt-5",
            "contextTokens": "128000",
            "apiKey": "sk-test",
        })
    );
    assert_eq!(encoded(&SummariesUpdateParams::default()), json!({}));
}

#[test]
fn export_update_params_every_field() {
    let params = ExportUpdateParams {
        people_folder: Some("People".into()),
        include_audio: Some(true),
        task_tag: Some("#todo".into()),
    };
    assert_eq!(
        encoded(&params),
        json!({"peopleFolder": "People", "includeAudio": true, "taskTag": "#todo"})
    );
    assert_eq!(encoded(&ExportUpdateParams::default()), json!({}));
}
