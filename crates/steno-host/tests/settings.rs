//! The Settings window's behaviour through the host, ported from
//! `SettingsBridgeTests`, `SettingsViewModelTests` and `SettingsSectionTests`.

// The ported suites keep one Swift test per function, long as some are.
#![allow(clippy::too_many_lines)]

mod common;

use std::sync::{Arc, Condvar, Mutex};

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

    harness
        .fakes
        .login_item
        .fail_changes(Some("SMAppService refused"));
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
            fakes.pipeline.set_kept_forever(3);
            fakes.folder_usage.set_bytes(Ok(4_200));
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
            fakes.folder_usage.set_bytes(Err("permission denied"));
            fakes.audio_devices.fail_listing(Some("Core Audio said no"));
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

/// `settings.transcription.download` replies at once; the download runs on
/// its own thread and publishes as it goes (Swift: `SpeechSettingsViewModel
/// .download`'s task).
#[test]
fn the_download_reply_returns_while_the_download_runs() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let harness = Harness::builder()
        .seed({
            let gate = gate.clone();
            move |_, fakes| {
                fakes.speech_models.set_on_download(move |_| {
                    let (open, signal) = &*gate;
                    let mut open = open.lock().unwrap();
                    while !*open {
                        open = signal.wait(open).unwrap();
                    }
                });
            }
        })
        .build();
    harness.sink.clear();
    let host = harness.host.clone();
    within_five_seconds("the download reply", move || {
        host.settings_transcription_download(AssetIdParams {
            asset_id: "offlineDiarizer".to_owned(),
        })
    })
    .unwrap();
    // Back here while the fake is still held on the gate.
    let asset = &harness.snapshot(BridgeTopic::SettingsTranscription)["assets"][1];
    assert_eq!(asset["state"], "downloading");
    assert_eq!(
        harness
            .sink
            .last(BridgeTopic::SettingsTranscription)
            .unwrap()["assets"][1]["state"],
        "downloading"
    );
    assert!(
        !harness
            .fakes
            .speech_models
            .is_installed(ModelAsset::OfflineDiarizer)
    );
    // A second download of the same asset while one runs is a no-op.
    harness
        .host
        .settings_transcription_download(AssetIdParams {
            asset_id: "offlineDiarizer".to_owned(),
        })
        .unwrap();
    {
        let (open, signal) = &*gate;
        *open.lock().unwrap() = true;
        signal.notify_all();
    }
    harness.wait_for_download(1);
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsTranscription)["assets"][1]["state"],
        "installed"
    );
    assert_eq!(
        harness.fakes.speech_models.downloads.lock().unwrap().len(),
        1
    );
}

/// A remove while the download runs detaches it: its late progress does
/// not mark the asset downloading again, and a second download of the
/// asset waits until the first thread has ended, as Swift's `downloads`
/// guard kept one task per asset.
#[test]
fn a_remove_detaches_the_download_in_flight() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let harness = Harness::builder()
        .seed({
            let gate = gate.clone();
            move |_, fakes| {
                fakes.speech_models.set_on_download(move |_| {
                    let (open, signal) = &*gate;
                    let mut open = open.lock().unwrap();
                    while !*open {
                        open = signal.wait(open).unwrap();
                    }
                });
            }
        })
        .build();
    let download = || {
        harness
            .host
            .settings_transcription_download(AssetIdParams {
                asset_id: "offlineDiarizer".to_owned(),
            })
            .unwrap();
    };
    download();
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
    harness.sink.clear();
    download();
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsTranscription)["assets"][1]["state"],
        "absent",
        "the first download still runs, so the second does not start"
    );
    {
        let (open, signal) = &*gate;
        *open.lock().unwrap() = true;
        signal.notify_all();
    }
    harness.wait_for("the detached download to end", |harness| {
        harness
            .host
            .snapshot(BridgeTopic::SettingsTranscription)
            .unwrap()["assets"][1]["state"]
            == "installed"
    });
    assert!(
        harness
            .sink
            .all(BridgeTopic::SettingsTranscription)
            .iter()
            .all(|snapshot| snapshot["assets"][1]["state"] != "downloading"),
        "a detached download's progress is not shown"
    );
    assert_eq!(
        harness.fakes.speech_models.downloads.lock().unwrap().len(),
        1
    );
    // Its thread has ended: the asset downloads again.
    download();
    harness.wait_for("the second download to run", |harness| {
        harness.fakes.speech_models.downloads.lock().unwrap().len() == 2
    });
    harness.wait_for_download(1);
}

/// `settings.recording.refreshDevices` re-reads the input list.
#[test]
fn refresh_devices_rereads_the_inputs() {
    let harness = Harness::builder().build();
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsRecording)["devices"],
        json!([])
    );
    harness
        .fakes
        .audio_devices
        .devices
        .lock()
        .unwrap()
        .push(steno_host::services::InputDevice {
            uid: "mic-1".to_owned(),
            name: "MacBook Pro Microphone".to_owned(),
        });
    harness.sink.clear();
    harness.host.settings_recording_refresh_devices().unwrap();
    let recording = harness.sink.last(BridgeTopic::SettingsRecording).unwrap();
    assert_eq!(
        recording["devices"],
        json!([{"name": "MacBook Pro Microphone", "uid": "mic-1"}])
    );
}

/// The page sees `isTesting` while the probe is out and `isRequesting`
/// while the prompt is up: the host publishes before it calls the service,
/// with its lock released. Swift: the awaited `test()` and `request`.
#[test]
fn busy_flags_are_published_before_the_service_runs() {
    let seen_testing = Arc::new(Mutex::new(None));
    let seen_requesting = Arc::new(Mutex::new(None));
    let harness = Harness::builder().build();
    harness.fakes.llm.set_on_probe({
        let sink = harness.sink.clone();
        let seen = seen_testing.clone();
        move |_| {
            *seen.lock().unwrap() = sink
                .last(BridgeTopic::SettingsSummaries)
                .map(|snapshot| snapshot["isTesting"].clone());
        }
    });
    harness
        .host
        .settings_summaries_update(update(Some("qwen3-8b"), None, None, None))
        .unwrap();
    harness.host.settings_summaries_test().unwrap();
    assert_eq!(*seen_testing.lock().unwrap(), Some(json!(true)));
    let summaries = harness.snapshot(BridgeTopic::SettingsSummaries);
    assert_eq!(summaries["isTesting"], false);
    assert_eq!(summaries["testResult"]["ok"], true);

    harness
        .fakes
        .permissions
        .set_state(PermissionKind::SystemAudio, PermissionState::Denied);
    harness
        .fakes
        .permissions
        .set_answer(PermissionKind::SystemAudio, PermissionState::Granted);
    harness.fakes.permissions.set_on_request({
        let sink = harness.sink.clone();
        let seen = seen_requesting.clone();
        move |_| {
            *seen.lock().unwrap() = sink
                .last(BridgeTopic::SettingsRecording)
                .map(|snapshot| snapshot["permissions"][1]["isRequesting"].clone());
        }
    });
    harness
        .host
        .settings_recording_request_permission(PermissionKindParams {
            kind: PermissionKind::SystemAudio,
        })
        .unwrap();
    assert_eq!(*seen_requesting.lock().unwrap(), Some(json!(true)));
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsRecording)["permissions"][1],
        json!({"isRequesting": false, "kind": "systemAudio", "state": "granted"})
    );
}

/// The shell's `bridge_call` is an `async` Tauri command, so a host call
/// can arrive inside a tokio runtime. The host awaits the secret store at
/// construction, on Save and when the sections reload; from there that
/// must block like any other call, not panic with "Cannot start a runtime
/// from within a runtime".
#[test]
fn the_secret_store_is_awaited_from_inside_an_async_runtime() {
    let runtimes = [
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap(),
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .unwrap(),
    ];
    for runtime in runtimes {
        runtime.block_on(async {
            let harness = Harness::builder().build();
            harness
                .host
                .settings_summaries_update(update(Some("gpt-4.1-mini"), None, None, None))
                .unwrap();
            harness
                .host
                .settings_summaries_select_preset(SetStringParams {
                    value: "openAI".to_owned(),
                })
                .unwrap();
            harness
                .host
                .settings_summaries_update(update(None, None, Some("sk-async"), None))
                .unwrap();
            harness.host.settings_summaries_save().unwrap();
            let stored = harness
                .fakes
                .secrets
                .secret(&SecretKey::llm_api_key())
                .await
                .unwrap();
            assert_eq!(stored.as_deref(), Some("sk-async"));

            // A write from elsewhere reloads the sections, the key with them.
            let mut settings = harness.store.settings().unwrap();
            settings.meeting_detection_enabled = false;
            harness.store.save_settings(&settings).unwrap();
            harness.host.store_changed();
            assert_eq!(
                harness.snapshot(BridgeTopic::SettingsGeneral)["detectionEnabled"],
                false
            );
        });
    }
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

    harness.fakes.llm.set_probe_result(Err("401 Unauthorized"));
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
            fakes
                .llm
                .set_codex_account(Ok("nicolai@example.com (Plus)"));
            fakes.llm.set_codex_models(Ok(vec![CodexModel {
                slug: "gpt-5.1-codex".to_owned(),
                display_name: "GPT-5.1 Codex".to_owned(),
                context_window: Some(272_000),
            }]));
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

    // The picker: the slug and its listed context window are stored.
    harness
        .fakes
        .llm
        .codex_models
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .push(CodexModel {
            slug: "gpt-5.1-codex-mini".to_owned(),
            display_name: "GPT-5.1 Codex Mini".to_owned(),
            context_window: Some(128_000),
        });
    harness
        .host
        .settings_summaries_refresh_codex_models()
        .unwrap();
    harness
        .host
        .settings_summaries_select_codex_model(SetStringParams {
            value: "gpt-5.1-codex-mini".to_owned(),
        })
        .unwrap();
    let settings = harness.store.settings().unwrap();
    assert_eq!(settings.codex_model.as_deref(), Some("gpt-5.1-codex-mini"));
    assert_eq!(settings.codex_context_tokens, 128_000);
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsSummaries)["codex"]["model"],
        "gpt-5.1-codex-mini"
    );

    // The explicit status refresh re-reads the sign-in.
    harness.fakes.llm.set_codex_account(Err("signed out"));
    harness
        .host
        .settings_summaries_refresh_codex_status()
        .unwrap();
    let summaries = harness.snapshot(BridgeTopic::SettingsSummaries);
    assert_eq!(summaries["codex"]["signIn"], "unavailable");
    assert_eq!(summaries["codex"]["signInDetail"], "signed out");

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

    // `settings.export.save` stores the typed fields, trimmed.
    harness
        .host
        .settings_export_update(ExportUpdateParams {
            people_folder: None,
            include_audio: None,
            task_tag: Some(" todo ".to_owned()),
        })
        .unwrap();
    assert_eq!(
        harness.store.settings().unwrap().obsidian.unwrap().task_tag,
        None,
        "typing stores nothing"
    );
    harness.host.settings_export_save().unwrap();
    assert_eq!(
        harness
            .store
            .settings()
            .unwrap()
            .obsidian
            .unwrap()
            .task_tag
            .as_deref(),
        Some("todo")
    );

    harness
        .fakes
        .export_validator
        .fail_validation(Some("The Obsidian vault at /x does not exist."));
    harness
        .host
        .settings_export_update(ExportUpdateParams {
            people_folder: Some("Elsewhere".to_owned()),
            include_audio: None,
            task_tag: None,
        })
        .unwrap();
    harness.host.settings_export_save().unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsExport)["validationMessage"],
        "The Obsidian vault at /x does not exist."
    );
    assert_eq!(
        harness
            .store
            .settings()
            .unwrap()
            .obsidian
            .unwrap()
            .people_folder
            .as_deref(),
        Some("People"),
        "a refused save stores nothing"
    );
    harness
        .host
        .settings_export_update(ExportUpdateParams {
            people_folder: Some("People".to_owned()),
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
        4
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
            fakes.qr.set_png(Some("iVBORw0KGgo="));
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

    // Cancel while the code is up: the code and the listener go, nothing
    // paired.
    harness.host.settings_phone_begin_pairing().unwrap();
    assert!(
        harness
            .snapshot(BridgeTopic::SettingsPhone)
            .get("pairing")
            .is_some()
    );
    harness.host.settings_phone_cancel_pairing().unwrap();
    let phone = harness.snapshot(BridgeTopic::SettingsPhone);
    assert!(phone.get("pairing").is_none());
    assert_eq!(phone["listener"]["state"], "stopped");
    assert!(
        harness
            .fakes
            .handover
            .as_ref()
            .unwrap()
            .pairing
            .lock()
            .unwrap()
            .is_none(),
        "the service's code is closed"
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
