//! The optional fields no fixture sets: each appears under its camelCase key
//! when set and is absent when `None`, as `contract.ts` declares.

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

/// `name`'s fixture leaves each pointer unset; after `set` each reads its value.
fn sets<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(
    name: &str,
    set: impl FnOnce(&mut T),
    expected: &[(&str, Value)],
) {
    let mut value: T = fixture(name);
    let before = encoded(&value);
    set(&mut value);
    let after = encoded(&value);
    for (pointer, wanted) in expected {
        assert!(
            before.pointer(pointer).is_none(),
            "{name}: {pointer} is set"
        );
        assert_eq!(after.pointer(pointer), Some(wanted), "{name}: {pointer}");
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn optional_snapshot_fields_appear_only_when_set() {
    sets(
        "app",
        |app: &mut AppSnapshot| {
            app.requested_meeting_id = Some(uuid(0x2a));
            app.requested_settings_section = Some(SettingsSection::Phone);
        },
        &[
            (
                "/requestedMeetingID",
                json!("00000000-0000-0000-0000-00000000002A"),
            ),
            ("/requestedSettingsSection", json!("iphone")),
        ],
    );
    sets(
        "recording.live",
        |recording: &mut RecordingSnapshot| {
            recording.warning = Some("The microphone went quiet.".into());
            recording.error = Some("System audio stopped.".into());
        },
        &[
            ("/warning", json!("The microphone went quiet.")),
            ("/error", json!("System audio stopped.")),
        ],
    );
    sets(
        "meetings.list",
        |list: &mut MeetingsListSnapshot| {
            list.tag_filter = Some("hiring".into());
            list.error = Some("The database is locked.".into());
        },
        &[
            ("/tagFilter", json!("hiring")),
            ("/error", json!("The database is locked.")),
        ],
    );
    sets(
        "meeting.detail",
        |detail: &mut MeetingDetailSnapshot| {
            detail.end_reason = Some("Stopped after 20 minutes of silence.".into());
            detail.error = Some("The notes could not be saved.".into());
        },
        &[
            ("/endReason", json!("Stopped after 20 minutes of silence.")),
            ("/error", json!("The notes could not be saved.")),
        ],
    );
    sets(
        "onboarding.handoff",
        |onboarding: &mut OnboardingSnapshot| {
            let row = onboarding.speech_models.as_mut().unwrap();
            row.download_fraction = Some(0.5);
            row.failure = Some("The network connection was lost.".into());
        },
        &[
            ("/speechModels/downloadFraction", json!(0.5)),
            (
                "/speechModels/failure",
                json!("The network connection was lost."),
            ),
        ],
    );
    sets(
        "settings.general",
        |general: &mut GeneralSettingsSnapshot| {
            general.error_details = Some("SMAppService returned 1".into());
            general.login_item_note = Some("Your system opens Steno.".into());
            general.updates.managed_note = Some("Updates come from your package manager.".into());
        },
        &[
            ("/errorDetails", json!("SMAppService returned 1")),
            ("/loginItemNote", json!("Your system opens Steno.")),
            (
                "/updates/managedNote",
                json!("Updates come from your package manager."),
            ),
        ],
    );
    sets(
        "settings.recording",
        |recording: &mut RecordingSettingsSnapshot| {
            recording.input_device_uid = Some("BuiltInMicrophoneDevice".into());
            recording.error_details = Some("CoreAudio -10851".into());
        },
        &[
            ("/inputDeviceUID", json!("BuiltInMicrophoneDevice")),
            ("/errorDetails", json!("CoreAudio -10851")),
        ],
    );
    sets(
        "settings.transcription",
        |transcription: &mut TranscriptionSettingsSnapshot| {
            transcription.error_details = Some("The model store is read only.".into());
        },
        &[("/errorDetails", json!("The model store is read only."))],
    );
    sets(
        "settings.summaries",
        |summaries: &mut SummariesSettingsSnapshot| {
            summaries.validation_message = Some("Enter a model name.".into());
            summaries.error_details = Some("401 Unauthorized".into());
        },
        &[
            ("/validationMessage", json!("Enter a model name.")),
            ("/errorDetails", json!("401 Unauthorized")),
        ],
    );
    sets(
        "settings.export",
        |export: &mut ExportSettingsSnapshot| {
            export.vault_path = Some("/Users/nicolai/Notes".into());
            export.vault_name = Some("Notes".into());
            export.validation_message = Some("The people folder must be inside the vault.".into());
            export.error_details = Some("EACCES: permission denied".into());
        },
        &[
            ("/vaultPath", json!("/Users/nicolai/Notes")),
            ("/vaultName", json!("Notes")),
            (
                "/validationMessage",
                json!("The people folder must be inside the vault."),
            ),
            ("/errorDetails", json!("EACCES: permission denied")),
        ],
    );
    sets(
        "settings.iphone",
        |phone: &mut PhoneSettingsSnapshot| {
            phone.error_details = Some("Bonjour registration failed (-72000)".into());
        },
        &[(
            "/errorDetails",
            json!("Bonjour registration failed (-72000)"),
        )],
    );
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
fn general_updates_last_check_and_detail() {
    let updates = GeneralUpdates {
        can_check: true,
        automatically_checks: true,
        automatically_downloads: false,
        last_check_at: Some(Utc.with_ymd_and_hms(2026, 9, 29, 12, 48, 0).unwrap()),
        outcome: GeneralUpdatesOutcome::Failed,
        detail: Some("The update server did not answer.".into()),
        managed_note: None,
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
