//! The Settings window's behaviour through the host, ported from
//! `SettingsBridgeTests`, `SettingsViewModelTests` and `SettingsSectionTests`.

// The ported suites keep one Swift test per function, long as some are.
#![allow(clippy::too_many_lines)]

mod common;

use steno_host::services::{LoginItem as _, SpeechModels as _};

use common::*;
use serde_json::json;
use steno_bridge::{
    AssetIdParams, BridgeErrorCode, BridgeHost, BridgeTopic, DeviceIdParams, ExportUpdateParams,
    PermissionKind, PermissionKindParams, PermissionState, RecordingRetention, RetentionMode,
    SetAutomaticUpdatesParams, SetBoolParams, SetRetentionParams, SetStringParams,
    SetTemplateParams, SummariesUpdateParams,
};
use steno_core::paths::file_url;
use steno_core::protocols::{SecretKey, SecretStore as _};
use steno_core::{AudioRetention, LlmProvider};
use steno_host::services::{CodexModel, LoginItemStatus};
use steno_host::speech::ModelAsset;

/// Swift: `pageReadyPublishesEveryTopicOnce` and `testOverviewSubtitles`.
#[test]
fn the_sections_carry_their_sidebar_subtitles() {
    let harness = Harness::builder().build();
    let subtitle = |topic| {
        harness.snapshot(topic)["subtitle"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(subtitle(BridgeTopic::SettingsGeneral), "Steno 0.10.0");
    assert_eq!(subtitle(BridgeTopic::SettingsRecording), "Ready");
    assert_eq!(
        subtitle(BridgeTopic::SettingsTranscription),
        "Download needed"
    );
    assert_eq!(subtitle(BridgeTopic::SettingsSummaries), "Not set up");
    assert_eq!(subtitle(BridgeTopic::SettingsExport), "Off");
    assert_eq!(subtitle(BridgeTopic::SettingsPhone), "Unavailable");
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsPhone)["listener"]["state"],
        "unavailable"
    );
}

/// Swift: `testGeneralSavesTemplateDetectionAndLoginItem`, `generalAndRecordingCommandsStoreAndRepublish`,
/// `testGeneralShowsCalendarAndUpdates`.
#[test]
fn general_saves_template_detection_login_item_calendar_and_updates() {
    let harness = Harness::builder().build();
    let general = harness.snapshot(BridgeTopic::SettingsGeneral);
    assert_eq!(general["defaultTemplateID"], "default");
    assert_eq!(general["detectionEnabled"], true);
    assert_eq!(general["loginItem"], "notRegistered");

    harness.sink.clear();
    harness
        .host
        .settings_general_set_default_template(SetTemplateParams {
            template_id: "daily-standup".to_owned(),
        })
        .unwrap();
    harness
        .host
        .settings_general_set_default_template(SetTemplateParams {
            template_id: "nope".to_owned(),
        })
        .unwrap();
    harness
        .host
        .settings_general_set_detection(SetBoolParams { value: false })
        .unwrap();
    harness
        .host
        .settings_general_set_launch_at_login(SetBoolParams { value: true })
        .unwrap();
    let settings = harness.store.settings().unwrap();
    assert_eq!(settings.default_template_id, "daily-standup");
    assert!(!settings.meeting_detection_enabled);
    assert!(settings.launch_at_login);
    assert_eq!(harness.fakes.login_item.status(), LoginItemStatus::Enabled);
    let general = harness.sink.last(BridgeTopic::SettingsGeneral).unwrap();
    assert_eq!(general["loginItem"], "enabled");
    assert_eq!(general["defaultTemplateID"], "daily-standup");
    assert!(general.get("error").is_none());

    *harness.fakes.login_item.failure.lock().unwrap() = Some("SMAppService refused".to_owned());
    harness
        .host
        .settings_general_set_launch_at_login(SetBoolParams { value: false })
        .unwrap();
    let general = harness.sink.last(BridgeTopic::SettingsGeneral).unwrap();
    assert_eq!(
        general["error"],
        "Opening Steno at login could not be changed."
    );
    assert_eq!(general["errorDetails"], "SMAppService refused");

    harness
        .fakes
        .permissions
        .set_state(PermissionKind::Calendar, PermissionState::Unknown);
    harness.host.settings_general_request_calendar().unwrap();
    assert_eq!(
        *harness.fakes.permissions.requests.lock().unwrap(),
        vec![PermissionKind::Calendar]
    );
    let general = harness.sink.last(BridgeTopic::SettingsGeneral).unwrap();
    assert_eq!(general["calendarPermission"], "granted");
    assert_eq!(general["requestingCalendar"], false);

    harness
        .host
        .settings_general_set_automatic_updates(SetAutomaticUpdatesParams {
            automatically_checks: false,
            automatically_downloads: true,
        })
        .unwrap();
    let updates = &harness.sink.last(BridgeTopic::SettingsGeneral).unwrap()["updates"];
    assert_eq!(updates["automaticallyChecks"], false);
    assert_eq!(updates["automaticallyDownloads"], true);
    harness.host.updates_check().unwrap();
    assert_eq!(*harness.fakes.updater.checks.lock().unwrap(), 1);
    harness.host.settings_general_open_login_items().unwrap();
    assert_eq!(*harness.fakes.login_item.opened.lock().unwrap(), 1);
}

/// Swift: `testAudioSavesDeviceFolderAndRetention`, `testAudioSwitchingToForeverKeepsRecordingsOnDisk`,
/// `chooseFolderAppliesThePanelsAnswer`, `testAudioShowsPermissionsAndFolderSize`.
#[test]
fn recording_saves_device_folder_and_retention_and_the_chooser_applies_its_answer() {
    let chosen = tempfile::tempdir().unwrap();
    let chosen_path = chosen.path().join("rec").to_string_lossy().into_owned();
    let harness = Harness::builder()
        .choose(Some(&chosen_path))
        .seed(|_, fakes| {
            *fakes.pipeline.kept_forever.lock().unwrap() = 3;
            *fakes.folder_usage.bytes.lock().unwrap() = Ok(4_200);
        })
        .build();
    let recording = harness.snapshot(BridgeTopic::SettingsRecording);
    assert_eq!(
        recording["retention"],
        json!({"days": 30, "mode": "keepForever"}),
        "a fresh install keeps every recording"
    );
    assert!(
        recording["retentionFootnote"]
            .as_str()
            .unwrap()
            .starts_with("Recordings stay in the folder above")
    );
    assert_eq!(recording["folderUsage"], "measured");
    assert_eq!(recording["folderUsageBytes"], 4_200);
    assert_eq!(recording["audioFolderName"], "audio");
    assert_eq!(
        recording["permissions"][0],
        json!({"isRequesting": false, "kind": "microphone", "state": "granted"})
    );

    harness
        .host
        .settings_recording_set_input_device(SetStringParams {
            value: "mic-1".to_owned(),
        })
        .unwrap();
    assert_eq!(
        harness
            .store
            .settings()
            .unwrap()
            .input_device_uid
            .as_deref(),
        Some("mic-1")
    );
    harness
        .host
        .settings_recording_set_input_device(SetStringParams {
            value: String::new(),
        })
        .unwrap();
    assert_eq!(
        harness.store.settings().unwrap().input_device_uid,
        None,
        "empty means the system default"
    );

    harness
        .host
        .settings_recording_set_retention(SetRetentionParams {
            retention: RecordingRetention {
                mode: RetentionMode::KeepDays,
                days: 0,
            },
        })
        .unwrap();
    assert_eq!(
        harness.store.settings().unwrap().default_retention,
        AudioRetention::KeepDays(1),
        "days clamp to the range"
    );
    assert!(
        harness
            .snapshot(BridgeTopic::SettingsRecording)
            .get("keptForeverCount")
            .is_none()
    );
    harness
        .host
        .settings_recording_set_retention(SetRetentionParams {
            retention: RecordingRetention {
                mode: RetentionMode::KeepForever,
                days: 30,
            },
        })
        .unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsRecording)["keptForeverCount"],
        3,
        "Forever keeps what is on disk"
    );
    harness
        .host
        .settings_recording_set_retention(SetRetentionParams {
            retention: RecordingRetention {
                mode: RetentionMode::DeleteAfterProcessing,
                days: 30,
            },
        })
        .unwrap();
    assert_eq!(
        harness.store.settings().unwrap().default_retention,
        AudioRetention::DeleteAfterProcessing
    );

    let reply = harness.host.settings_recording_choose_folder().unwrap();
    assert_eq!(reply.path.as_deref(), Some(chosen_path.as_str()));
    assert_eq!(
        harness.store.settings().unwrap().audio_folder,
        file_url(std::path::Path::new(&chosen_path), true)
    );
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsRecording)["audioFolderName"],
        "rec"
    );
    assert_eq!(
        harness
            .fakes
            .folder_usage
            .measured
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .to_string_lossy(),
        chosen_path
    );

    let cancelling = Harness::builder().choose(None).build();
    let before = cancelling.store.settings().unwrap().audio_folder;
    let reply = cancelling.host.settings_recording_choose_folder().unwrap();
    assert_eq!(reply.path, None);
    assert_eq!(cancelling.store.settings().unwrap().audio_folder, before);

    harness
        .fakes
        .permissions
        .set_state(PermissionKind::SystemAudio, PermissionState::Denied);
    harness
        .fakes
        .permissions
        .set_answer(PermissionKind::SystemAudio, PermissionState::Denied);
    harness
        .host
        .settings_recording_request_permission(PermissionKindParams {
            kind: PermissionKind::SystemAudio,
        })
        .unwrap();
    let recording = harness.snapshot(BridgeTopic::SettingsRecording);
    assert_eq!(recording["permissions"][1]["state"], "denied");
    assert_eq!(recording["subtitle"], "Permission needed");
    harness.host.settings_recording_reveal_folder().unwrap();
    assert_eq!(harness.fakes.opener.revealed.lock().unwrap().len(), 1);
}

/// Swift: `testAudioReportsAnUnreadableFolderAsUnavailable`, `testErrorsAreSentencesWithDetailsApart`.
#[test]
fn errors_are_sentences_with_the_details_apart() {
    let harness = Harness::builder()
        .seed(|_, fakes| {
            *fakes.folder_usage.bytes.lock().unwrap() = Err("permission denied".to_owned());
            *fakes.audio_devices.failure.lock().unwrap() = Some("Core Audio said no".to_owned());
        })
        .build();
    let recording = harness.snapshot(BridgeTopic::SettingsRecording);
    assert_eq!(recording["folderUsage"], "unavailable");
    assert!(recording.get("folderUsageBytes").is_none());
    assert_eq!(recording["error"], "Microphones could not be listed.");
    assert_eq!(recording["errorDetails"], "Core Audio said no");
    assert_eq!(recording["devices"], json!([]));
}

/// Swift: `testSpeechEngineChangeReloadsThePipeline`, `testSpeechDownloadStreamsProgressToInstalled`.
#[test]
fn transcription_switches_engines_and_downloads_assets() {
    let harness = Harness::builder().build();
    let transcription = harness.snapshot(BridgeTopic::SettingsTranscription);
    assert_eq!(transcription["engineID"], "parakeet-v3");
    assert_eq!(
        transcription["engines"][0]["name"],
        "Parakeet · fast · 25 languages"
    );
    assert_eq!(transcription["assets"][0]["id"], "parakeetV3");
    assert_eq!(
        transcription["assets"][0]["detail"],
        "Not downloaded · 485 MB"
    );
    assert_eq!(transcription["allInstalled"], false);

    harness
        .host
        .settings_transcription_set_engine(SetStringParams {
            value: "whisperkit-large-v3-turbo".to_owned(),
        })
        .unwrap();
    assert_eq!(
        harness.store.settings().unwrap().speech_engine_id,
        "whisperkit-large-v3-turbo"
    );
    assert_eq!(*harness.fakes.pipeline.reloads.lock().unwrap(), 1);
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsTranscription)["assets"][0]["id"],
        "whisperLargeV3Turbo"
    );
    let unknown = harness
        .host
        .settings_transcription_set_engine(SetStringParams {
            value: "nope".to_owned(),
        })
        .unwrap_err();
    assert_eq!(unknown.code, BridgeErrorCode::InvalidParams);

    harness.sink.clear();
    harness
        .host
        .settings_transcription_download(AssetIdParams {
            asset_id: "offlineDiarizer".to_owned(),
        })
        .unwrap();
    harness.wait_for_download(1);
    let states: Vec<String> = harness
        .sink
        .all(BridgeTopic::SettingsTranscription)
        .iter()
        .map(|snapshot| snapshot["assets"][1]["state"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(states.first().map(String::as_str), Some("downloading"));
    assert_eq!(states.last().map(String::as_str), Some("installed"));
    assert!(
        harness
            .fakes
            .speech_models
            .is_installed(ModelAsset::OfflineDiarizer)
    );
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsTranscription)["assets"][1]["installedBytes"],
        22_000_000
    );

    harness
        .host
        .settings_transcription_remove(AssetIdParams {
            asset_id: "offlineDiarizer".to_owned(),
        })
        .unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsTranscription)["assets"][1]["state"],
        "absent"
    );
    let unknown = harness
        .host
        .settings_transcription_download(AssetIdParams {
            asset_id: "nope".to_owned(),
        })
        .unwrap_err();
    assert_eq!(unknown.code, BridgeErrorCode::InvalidParams);

    harness.fakes.speech_models.fail_downloads(Some("offline"));
    harness
        .host
        .settings_transcription_download(AssetIdParams {
            asset_id: "offlineDiarizer".to_owned(),
        })
        .unwrap();
    harness.wait_for_download(1);
    let asset = &harness.snapshot(BridgeTopic::SettingsTranscription)["assets"][1];
    assert_eq!(asset["state"], "failed");
    assert_eq!(asset["failure"], "offline");
}

fn update(
    model: Option<&str>,
    url: Option<&str>,
    key: Option<&str>,
    tokens: Option<&str>,
) -> SummariesUpdateParams {
    SummariesUpdateParams {
        base_url: url.map(str::to_owned),
        model: model.map(str::to_owned),
        context_tokens: tokens.map(str::to_owned),
        api_key: key.map(str::to_owned),
    }
}

/// Swift: `testLLMValidatesAndSavesSettingsAndKey`, `testLLMInvalidInputSavesNothing`,
/// `testLLMCommitSavesOnlyChangesThenProbes`, `testLLMSelectPresetFillsAndStoresTheAddress`,
/// `summariesAndExportDraftsStoreOnSave`.
#[test]
fn summaries_validate_save_the_key_apart_and_probe() {
    let harness = Harness::builder().build();
    harness
        .host
        .settings_summaries_update(update(Some("gpt-4.1-mini"), Some("not a url"), None, None))
        .unwrap();
    harness.host.settings_summaries_save().unwrap();
    let summaries = harness.snapshot(BridgeTopic::SettingsSummaries);
    assert_eq!(
        summaries["validationMessage"],
        "The server address must start with http:// or https:// and name a host."
    );
    assert_eq!(
        harness.store.settings().unwrap().llm_model,
        None,
        "invalid input saves nothing"
    );

    harness
        .host
        .settings_summaries_select_preset(SetStringParams {
            value: "openAI".to_owned(),
        })
        .unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsSummaries)["baseURL"],
        "https://api.openai.com/v1"
    );
    assert_eq!(
        harness.store.settings().unwrap().llm_base_url.as_deref(),
        Some("https://api.openai.com/v1"),
        "the preset's address is stored right away"
    );
    assert_eq!(
        harness.store.settings().unwrap().llm_model.as_deref(),
        Some("gpt-4.1-mini"),
        "the typed model saves along with the address"
    );
    assert_eq!(
        harness.fakes.llm.probes.lock().unwrap().len(),
        1,
        "a configured save probes"
    );

    harness
        .host
        .settings_summaries_update(update(None, None, Some(" sk-test "), Some("500")))
        .unwrap();
    harness.host.settings_summaries_save().unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsSummaries)["validationMessage"],
        "The context size must be at least 1024."
    );
    harness
        .host
        .settings_summaries_update(update(None, None, None, Some("16000")))
        .unwrap();
    harness.host.settings_summaries_save().unwrap();
    let settings = harness.store.settings().unwrap();
    assert_eq!(settings.llm_model.as_deref(), Some("gpt-4.1-mini"));
    assert_eq!(settings.llm_context_tokens, 16_000);
    let stored_key = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(harness.fakes.secrets.secret(&SecretKey::llm_api_key()))
        .unwrap();
    assert_eq!(
        stored_key.as_deref(),
        Some("sk-test"),
        "the key goes to the secret store, trimmed"
    );
    let rendered =
        steno_bridge::json::to_canonical_string(&harness.snapshot(BridgeTopic::SettingsSummaries))
            .unwrap();
    assert!(
        !rendered.contains("sk-test"),
        "the key never reaches a snapshot"
    );
    let summaries = harness.snapshot(BridgeTopic::SettingsSummaries);
    assert_eq!(summaries["hasAPIKey"], true);
    assert_eq!(summaries["isConfigured"], true);
    assert_eq!(
        summaries["testResult"]["ok"], true,
        "a configured save probes"
    );
    assert_eq!(summaries["subtitle"], "OpenAI");
    assert_eq!(
        *harness.fakes.pipeline.reloads.lock().unwrap(),
        2,
        "each save rebuilds the pipeline"
    );
    assert_eq!(harness.fakes.llm.probes.lock().unwrap().len(), 2);

    harness.host.settings_summaries_save().unwrap();
    assert_eq!(
        harness.fakes.llm.probes.lock().unwrap().len(),
        2,
        "an unchanged form saves and probes nothing"
    );

    *harness.fakes.llm.probe_result.lock().unwrap() = Err("401 Unauthorized".to_owned());
    harness.host.settings_summaries_test().unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsSummaries)["testResult"],
        json!({"message": "401 Unauthorized", "ok": false})
    );
}

/// Swift: `testCodexPresetIsOffUntilConfirmed`, `testClosingThePaneWithTheConsentCardOnScreenStoresNothing`,
/// `testConfirmingCodexWithoutASignInStaysUnconfigured`, `testCodexConfigurationRoundTripsAndKeepsTheEndpointFields`.
#[test]
fn the_codex_preset_stores_nothing_until_confirmed_and_picks_the_first_model() {
    let harness = Harness::builder()
        .seed(|_, fakes| {
            *fakes.llm.codex_account.lock().unwrap() = Ok("nicolai@example.com (Plus)".to_owned());
            *fakes.llm.codex_models.lock().unwrap() = Ok(vec![CodexModel {
                slug: "gpt-5.1-codex".to_owned(),
                display_name: "GPT-5.1 Codex".to_owned(),
                context_window: Some(272_000),
            }]);
        })
        .build();
    harness
        .host
        .settings_summaries_select_preset(SetStringParams {
            value: "codex".to_owned(),
        })
        .unwrap();
    let summaries = harness.snapshot(BridgeTopic::SettingsSummaries);
    assert_eq!(summaries["codex"]["confirmed"], false);
    assert_eq!(summaries["codex"]["signIn"], "signedIn");
    assert_eq!(
        summaries["codex"]["signInDetail"],
        "nicolai@example.com (Plus)"
    );
    assert_eq!(
        harness.store.settings().unwrap().llm_provider,
        LlmProvider::Endpoint,
        "the card stores nothing"
    );
    harness.host.settings_summaries_save().unwrap();
    assert_eq!(
        harness.store.settings().unwrap().llm_provider,
        LlmProvider::Endpoint,
        "nor does leaving the pane"
    );

    harness.host.settings_summaries_confirm_codex().unwrap();
    let settings = harness.store.settings().unwrap();
    assert_eq!(settings.llm_provider, LlmProvider::Codex);
    assert!(settings.codex_confirmed_at.is_some());
    assert_eq!(settings.codex_model.as_deref(), Some("gpt-5.1-codex"));
    assert_eq!(settings.codex_context_tokens, 272_000);
    let summaries = harness.snapshot(BridgeTopic::SettingsSummaries);
    assert_eq!(summaries["isConfigured"], true);
    assert_eq!(summaries["codex"]["model"], "gpt-5.1-codex");
    assert_eq!(summaries["subtitle"], "ChatGPT (Codex)");

    harness.host.settings_summaries_stop_using_codex().unwrap();
    let settings = harness.store.settings().unwrap();
    assert_eq!(settings.llm_provider, LlmProvider::Endpoint);
    assert!(settings.codex_confirmed_at.is_none());
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsSummaries)["presetID"],
        "lmStudio"
    );

    let unsigned = Harness::builder().build();
    unsigned
        .host
        .settings_summaries_select_preset(SetStringParams {
            value: "codex".to_owned(),
        })
        .unwrap();
    unsigned.host.settings_summaries_confirm_codex().unwrap();
    let summaries = unsigned.snapshot(BridgeTopic::SettingsSummaries);
    assert_eq!(summaries["codex"]["confirmed"], true);
    assert_eq!(summaries["codex"]["signIn"], "unavailable");
    assert_eq!(
        summaries["isConfigured"], false,
        "no model without a sign-in"
    );
}

/// Swift: `testObsidianValidationSurfacesTheDestinationMessageVerbatim`,
/// `testObsidianDisabledSavesNilWithoutValidating`, `testObsidianCommitWaitsForAVaultAndSavesOnChoice`.
#[test]
fn export_waits_for_a_vault_validates_through_the_destination_and_saves() {
    let harness = Harness::builder()
        .choose(Some("/Users/nicolai/Notes/Work Vault"))
        .build();
    harness
        .host
        .settings_export_set_enabled(SetBoolParams { value: true })
        .unwrap();
    assert_eq!(
        harness.store.settings().unwrap().obsidian,
        None,
        "on without a vault waits for the chooser"
    );
    let export = harness.snapshot(BridgeTopic::SettingsExport);
    assert_eq!(export["enabled"], true);
    assert_eq!(export["saved"], false);

    harness
        .host
        .settings_export_update(ExportUpdateParams {
            people_folder: Some(" People ".to_owned()),
            include_audio: None,
            task_tag: Some(String::new()),
        })
        .unwrap();
    let reply = harness.host.settings_export_choose_vault().unwrap();
    assert_eq!(
        reply.path.as_deref(),
        Some("/Users/nicolai/Notes/Work Vault")
    );
    let obsidian = harness.store.settings().unwrap().obsidian.unwrap();
    assert_eq!(obsidian.vault_path, "/Users/nicolai/Notes/Work Vault");
    assert_eq!(obsidian.people_folder.as_deref(), Some("People"), "trimmed");
    assert_eq!(obsidian.task_tag, None, "blank is none");
    assert_eq!(
        harness
            .fakes
            .export_validator
            .validated
            .lock()
            .unwrap()
            .len(),
        1
    );
    let export = harness.snapshot(BridgeTopic::SettingsExport);
    assert_eq!(export["saved"], true);
    assert_eq!(export["vaultName"], "Work Vault");
    assert_eq!(export["subtitle"], "Work Vault");

    *harness.fakes.export_validator.failure.lock().unwrap() =
        Some("The Obsidian vault at /x does not exist.".to_owned());
    harness
        .host
        .settings_export_update(ExportUpdateParams {
            people_folder: None,
            include_audio: Some(true),
            task_tag: None,
        })
        .unwrap();
    let export = harness.snapshot(BridgeTopic::SettingsExport);
    assert_eq!(
        export["validationMessage"],
        "The Obsidian vault at /x does not exist."
    );
    assert!(
        !harness
            .store
            .settings()
            .unwrap()
            .obsidian
            .unwrap()
            .include_audio,
        "a refused draft stores nothing"
    );

    harness
        .host
        .settings_export_set_enabled(SetBoolParams { value: false })
        .unwrap();
    assert_eq!(
        harness.store.settings().unwrap().obsidian,
        None,
        "off saves none without validating"
    );
    assert_eq!(
        harness
            .fakes
            .export_validator
            .validated
            .lock()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsExport)["subtitle"],
        "Off"
    );
}

/// Swift: `testPhonesPairingListsDevicesAndRevokes`, `testPhonesWithoutAHandoverServiceIsUnavailable`.
#[test]
fn phones_pair_list_and_revoke_devices() {
    let harness = Harness::builder()
        .with_handover("steno-mac-7f3a", 52_431)
        .seed(|_, fakes| {
            *fakes.qr.png_base64.lock().unwrap() = Some("iVBORw0KGgo=".to_owned());
        })
        .build();
    let phone = harness.snapshot(BridgeTopic::SettingsPhone);
    assert_eq!(phone["listener"]["state"], "stopped");
    assert_eq!(phone["subtitle"], "No iPhone paired");
    assert_eq!(phone["macID"], "steno-mac-7f3a");

    harness.host.settings_phone_begin_pairing().unwrap();
    let phone = harness.snapshot(BridgeTopic::SettingsPhone);
    assert_eq!(
        phone["listener"],
        json!({"port": 52_431, "state": "listening"})
    );
    assert_eq!(phone["pairing"]["qrPNGBase64"], "iVBORw0KGgo=");
    assert_eq!(phone["pairing"]["expiresAt"], "2026-09-29T12:54:00.000Z");

    // The phone arrives: the poll closes the code.
    harness
        .fakes
        .handover
        .as_ref()
        .unwrap()
        .pair(paired_phone());
    harness.host.refresh_pairing();
    let phone = harness.snapshot(BridgeTopic::SettingsPhone);
    assert!(phone.get("pairing").is_none());
    assert_eq!(phone["devices"][0]["name"], "Nicolai's iPhone");
    assert_eq!(phone["subtitle"], "1 iPhone paired");

    harness
        .host
        .settings_phone_revoke(DeviceIdParams {
            device_id: uuid(PHONE),
        })
        .unwrap();
    let phone = harness.snapshot(BridgeTopic::SettingsPhone);
    assert_eq!(phone["devices"], json!([]));
    assert_eq!(
        phone["listener"]["state"], "stopped",
        "no phones, no listener"
    );

    // A code that runs out with no phone closes itself.
    harness.host.settings_phone_begin_pairing().unwrap();
    harness.fakes.clock.set(date("2026-09-29T12:55:00.000Z"));
    harness.host.refresh_pairing();
    assert!(
        harness
            .snapshot(BridgeTopic::SettingsPhone)
            .get("pairing")
            .is_none()
    );
    harness.host.settings_phone_cancel_pairing().unwrap();

    *harness
        .fakes
        .handover
        .as_ref()
        .unwrap()
        .start_failure
        .lock()
        .unwrap() = Some("port in use".to_owned());
    harness.host.settings_phone_begin_pairing().unwrap();
    let phone = harness.snapshot(BridgeTopic::SettingsPhone);
    assert_eq!(phone["error"], "Pairing could not start.");
    assert_eq!(phone["errorDetails"], "port in use");
}
