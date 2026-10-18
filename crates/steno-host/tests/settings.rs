//! The Settings window's behaviour through the host.

// Some tests are long.
#![allow(clippy::too_many_lines)]

mod common;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};

use steno_host::services::{LoginItem as _, Preferences as _, SpeechModels as _};

use common::*;
use serde_json::json;
use steno_bridge::{
    AssetIdParams, BridgeErrorCode, BridgeHost, BridgeTopic, DeviceIdParams, ExportUpdateParams,
    PermissionKind, PermissionKindParams, PermissionState, PhoneWaitingReason, Platform,
    RecordingRetention, RetentionMode, SetAutomaticUpdatesParams, SetBoolParams,
    SetRetentionParams, SetStringParams, SetTemplateParams, SummariesUpdateParams,
};
use steno_core::paths::file_url;
use steno_core::protocols::{SecretKey, SecretStore};
use steno_core::{AudioRetention, LlmProvider, MeetingEvent, MeetingState};
use steno_host::host::{LOGIN_ITEM_REGISTERED_KEY, MAIN_APP_REGISTERED_KEY, first_launch_key};
use steno_host::services::{CodexModel, LoginItemStatus, UpdateOutcome};
use steno_host::settings::KeyRead;
use steno_host::speech::ModelAsset;

/// Page ready publishes every topic once.
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

/// A check that ended outside a command reaches the General section and the
/// overview's subtitle through `updates_changed`.
#[test]
fn a_check_outside_a_command_republishes_the_general_section() {
    let harness = Harness::builder().build();
    harness.fakes.updater.set_last_check(
        Some(date("2026-10-09T08:00:00.000Z")),
        UpdateOutcome::Available("0.12.0".into()),
    );
    harness.sink.clear();
    harness.host.updates_changed();
    let general = harness.sink.last(BridgeTopic::SettingsGeneral).unwrap();
    assert_eq!(
        general["updates"]["lastCheckAt"],
        "2026-10-09T08:00:00.000Z"
    );
    assert_eq!(general["updates"]["outcome"], "available");
    assert_eq!(general["updates"]["detail"], "0.12.0");
    assert_eq!(general["subtitle"], "Update available: 0.12.0");
}

/// The desktop shell's refusal when no launcher lies at a path that
/// outlives an upgrade.
const NO_STABLE_PATH: &str = "Steno can't open at login from where it's installed now. Restart Steno, or install it with your package manager.";

fn launch_counted(harness: &Harness) -> bool {
    flag(harness, MAIN_APP_REGISTERED_KEY)
}

fn flag(harness: &Harness, key: &str) -> bool {
    harness.fakes.preferences.flag(key)
}

fn seed_launch_at_login(store: &steno_core::Store, _: &steno_host::fakes::FakeServices) {
    let mut settings = store.settings().unwrap();
    settings.launch_at_login = true;
    store.save_settings(&settings).unwrap();
}

/// The host after two launches on the Mac from an installed bundle.
fn launched_on_mac(on: bool, status: LoginItemStatus, flags: &'static [&'static str]) -> Harness {
    let harness = Harness::builder()
        .platform(Platform::Macos)
        .seed(move |store, fakes| {
            if on {
                seed_launch_at_login(store, fakes);
            }
            fakes.login_item.set_status(status);
            for key in flags {
                fakes.preferences.set_flag(key, true);
            }
        })
        .build();
    harness.host.register_login_item_on_first_launch();
    harness.host.register_login_item_on_first_launch();
    harness
}

/// The first launch on the Mac registers only a not-registered login item
/// with the setting on; the earlier builds' flag does not stop it, one
/// removed after it is not added back (only the switch in General does), and
/// a run outside an app bundle (`NotFound`) or with the setting off is not
/// counted.
#[test]
fn the_first_launch_on_the_mac_registers_only_a_not_registered_login_item() {
    use LoginItemStatus::{Enabled, NotFound, NotRegistered, RequiresApproval};
    let none: &[&str] = &[];
    for (on, status, flags, changes, counted) in [
        (
            true,
            NotRegistered,
            &[LOGIN_ITEM_REGISTERED_KEY][..],
            &[true][..],
            true,
        ),
        (
            true,
            NotRegistered,
            &[MAIN_APP_REGISTERED_KEY][..],
            &[][..],
            true,
        ),
        (false, NotRegistered, none, &[][..], false),
        (true, Enabled, none, &[][..], true),
        (true, RequiresApproval, none, &[][..], true),
        (true, NotFound, none, &[][..], false),
    ] {
        let harness = launched_on_mac(on, status, flags);
        let case = format!("{on} {status:?} {flags:?}");
        assert_eq!(
            *harness.fakes.login_item.changes.lock().unwrap(),
            changes,
            "{case}"
        );
        assert_eq!(launch_counted(&harness), counted, "{case}");
    }
}

#[test]
fn on_the_mac_only_an_installed_bundle_counts_the_first_launch() {
    let outside = Harness::builder()
        .outside_installed_bundle()
        .seed(seed_launch_at_login)
        .build();
    outside.host.register_login_item_on_first_launch();
    assert!(outside.fakes.login_item.changes.lock().unwrap().is_empty());
    assert!(!launch_counted(&outside));

    let preferences = outside.fakes.preferences.clone();
    let login_item = outside.fakes.login_item.clone();
    let installed = Harness::builder()
        .seed(seed_launch_at_login)
        .change_services(move |services| {
            services.preferences = preferences;
            services.login_item = login_item;
        })
        .build();
    installed.host.register_login_item_on_first_launch();
    assert_eq!(*outside.fakes.login_item.changes.lock().unwrap(), [true]);
    assert!(launch_counted(&outside));
}

/// Off the Mac the first launch keeps its own flag, whatever the Mac's flag
/// or the bundle; a login item reading `NotFound` counts there.
#[test]
fn off_the_mac_the_first_launch_is_counted_under_the_swift_flag() {
    for platform in [Platform::Linux, Platform::Windows] {
        let harness = Harness::builder()
            .platform(platform)
            .outside_installed_bundle()
            .seed(seed_launch_at_login)
            .seed(|_, fakes| fakes.preferences.set_flag(MAIN_APP_REGISTERED_KEY, true))
            .build();
        harness.host.register_login_item_on_first_launch();
        harness.host.register_login_item_on_first_launch();
        assert_eq!(*harness.fakes.login_item.changes.lock().unwrap(), [true]);
        assert!(flag(&harness, LOGIN_ITEM_REGISTERED_KEY));
        assert_eq!(first_launch_key(platform), LOGIN_ITEM_REGISTERED_KEY);

        let harness = Harness::builder()
            .platform(platform)
            .seed(seed_launch_at_login)
            .seed(|_, fakes| fakes.login_item.set_status(LoginItemStatus::NotFound))
            .build();
        harness.host.register_login_item_on_first_launch();
        assert!(harness.fakes.login_item.changes.lock().unwrap().is_empty());
        assert!(flag(&harness, LOGIN_ITEM_REGISTERED_KEY));
    }
    assert_eq!(first_launch_key(Platform::Macos), MAIN_APP_REGISTERED_KEY);
}

/// The first launch registers the login item once; one the system manages
/// is never touched, and that launch is not counted.
#[test]
fn the_first_launch_registers_the_login_item_unless_the_system_manages_it() {
    let harness = Harness::builder().seed(seed_launch_at_login).build();
    harness.host.register_login_item_on_first_launch();
    harness.host.register_login_item_on_first_launch();
    assert_eq!(*harness.fakes.login_item.changes.lock().unwrap(), [true]);
    assert!(launch_counted(&harness));

    let harness = Harness::builder()
        .seed(seed_launch_at_login)
        .seed(|_, fakes| fakes.login_item.set_status(LoginItemStatus::Managed))
        .build();
    harness.host.register_login_item_on_first_launch();
    assert!(harness.fakes.login_item.changes.lock().unwrap().is_empty());
    assert!(!launch_counted(&harness));
    assert_eq!(harness.fakes.login_item.status(), LoginItemStatus::Managed);

    // A registration that fails does not count the launch, so the next
    // one tries again.
    let harness = Harness::builder().seed(seed_launch_at_login).build();
    harness.fakes.login_item.fail_changes(Some(NO_STABLE_PATH));
    harness.host.register_login_item_on_first_launch();
    assert!(!launch_counted(&harness));
    harness.fakes.login_item.fail_changes(None);
    harness.host.register_login_item_on_first_launch();
    assert_eq!(*harness.fakes.login_item.changes.lock().unwrap(), [true]);
    assert!(launch_counted(&harness));
}

#[test]
fn a_login_item_the_system_manages_shows_on_and_does_not_switch() {
    let harness = Harness::builder()
        .seed(|_, fakes| fakes.login_item.set_status(LoginItemStatus::Managed))
        .build();
    let general = harness.snapshot(BridgeTopic::SettingsGeneral);
    assert_eq!(general["loginItem"], "enabled");
    assert_eq!(
        general["loginItemNote"],
        steno_host::settings::snapshots::MANAGED_LOGIN_ITEM
    );

    for value in [false, true] {
        harness
            .host
            .settings_general_set_launch_at_login(SetBoolParams { value })
            .unwrap();
    }
    assert!(harness.fakes.login_item.changes.lock().unwrap().is_empty());
    assert!(!harness.store.settings().unwrap().launch_at_login);
    let general = harness.snapshot(BridgeTopic::SettingsGeneral);
    assert_eq!(general["loginItem"], "enabled");
    assert!(general.get("error").is_none());

    let plain = Harness::builder().build();
    assert!(
        plain
            .snapshot(BridgeTopic::SettingsGeneral)
            .get("loginItemNote")
            .is_none()
    );

    // The system takes the login item over after the section was read: the
    // switch asks again.
    plain.snapshot(BridgeTopic::SettingsGeneral);
    plain.fakes.login_item.set_status(LoginItemStatus::Managed);
    plain
        .host
        .settings_general_set_launch_at_login(SetBoolParams { value: true })
        .unwrap();
    assert!(plain.fakes.login_item.changes.lock().unwrap().is_empty());
    assert!(!plain.store.settings().unwrap().launch_at_login);
}

#[test]
fn a_login_item_that_cannot_be_registered_says_why_and_is_not_saved() {
    let harness = Harness::builder()
        .seed(|_, fakes| fakes.login_item.fail_changes(Some(NO_STABLE_PATH)))
        .build();
    harness
        .host
        .settings_general_set_launch_at_login(SetBoolParams { value: true })
        .unwrap();
    assert!(!harness.store.settings().unwrap().launch_at_login);
    let general = harness.snapshot(BridgeTopic::SettingsGeneral);
    assert_eq!(general["loginItem"], "notRegistered");
    assert_eq!(general["errorDetails"], NO_STABLE_PATH);
}

/// The Updates row says where updates come from; `updates.check` checks nothing.
#[test]
fn a_packaged_install_offers_no_update_check() {
    let harness = Harness::builder().updates_managed().build();
    let updates = &harness.snapshot(BridgeTopic::SettingsGeneral)["updates"];
    assert_eq!(updates["canCheck"], false);
    assert_eq!(
        updates["managedNote"],
        steno_host::settings::snapshots::MANAGED_UPDATES
    );
    harness.host.updates_check().unwrap();
    assert_eq!(*harness.fakes.updater.checks.lock().unwrap(), 0);

    let plain = Harness::builder().build();
    let updates = &plain.snapshot(BridgeTopic::SettingsGeneral)["updates"];
    assert_eq!(updates["canCheck"], true);
    assert!(updates.get("managedNote").is_none());
}

/// Switching to forever keeps recordings on disk; the section shows the
/// permissions and the folder size.
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

    let leaving = harness.audio_folder();
    let reply = harness.host.settings_recording_choose_folder().unwrap();
    assert_eq!(reply.path.as_deref(), Some(chosen_path.as_str()));
    assert_eq!(
        *harness.fakes.recorder.remembered.lock().unwrap(),
        [leaving],
        "the folder left is kept for crash recovery"
    );
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
    assert!(
        cancelling
            .fakes
            .recorder
            .remembered
            .lock()
            .unwrap()
            .is_empty()
    );

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
    assert_eq!(
        *harness.fakes.opener.revealed.lock().unwrap(),
        vec![std::path::PathBuf::from(&chosen_path)],
        "the folder the chooser set"
    );
}

/// The warning shows, and a folder chosen after it is checked again.
#[test]
fn a_folder_on_a_drive_that_may_lose_recent_writes_shows_the_warning() {
    let chosen = tempfile::tempdir().unwrap();
    let chosen_path = chosen.path().join("rec").to_string_lossy().into_owned();
    let harness = Harness::builder()
        .choose(Some(&chosen_path))
        .seed(|_, fakes| *fakes.folder_usage.may_lose_recent_writes.lock().unwrap() = true)
        .build();
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsRecording)["audioFolderWarning"],
        "This drive may lose recent recordings in a power cut."
    );
    *harness
        .fakes
        .folder_usage
        .may_lose_recent_writes
        .lock()
        .unwrap() = false;
    harness.host.settings_recording_choose_folder().unwrap();
    assert!(
        harness
            .snapshot(BridgeTopic::SettingsRecording)
            .get("audioFolderWarning")
            .is_none()
    );
}

/// An unreadable folder reads as unavailable.
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

/// A harness whose speech sidecar stands in for the Neural Engine.
fn falling_back() -> Harness {
    Harness::builder()
        .seed(|_, fakes| {
            fakes
                .speech_models
                .speech_fallback
                .store(true, Ordering::SeqCst);
        })
        .build()
}

/// The offer ends the fallback, rebuilds the pipeline and goes.
#[test]
fn transcription_offers_the_neural_engine_again_after_the_fallback() {
    assert!(
        Harness::builder()
            .build()
            .snapshot(BridgeTopic::SettingsTranscription)
            .get("neuralEngineOff")
            .is_none(),
        "left out while the Neural Engine is on"
    );
    let harness = falling_back();
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsTranscription)["neuralEngineOff"],
        true
    );

    harness
        .host
        .settings_transcription_use_neural_engine()
        .unwrap();
    assert!(
        !harness
            .fakes
            .speech_models
            .speech_fallback
            .load(Ordering::SeqCst)
    );
    assert_eq!(*harness.fakes.pipeline.reloads.lock().unwrap(), 1);
    let transcription = harness.snapshot(BridgeTopic::SettingsTranscription);
    assert!(transcription.get("neuralEngineOff").is_none());
    assert!(transcription.get("error").is_none());
}

/// It says why, also after a refresh, and a second press tries again.
#[test]
fn a_failed_rebuild_after_the_fallback_ended_keeps_the_offer() {
    let harness = falling_back();
    harness.fakes.pipeline.fail_calls(Some("the store is busy"));
    harness
        .host
        .settings_transcription_use_neural_engine()
        .unwrap();
    assert_eq!(*harness.fakes.pipeline.reloads.lock().unwrap(), 1);
    let transcription = harness.snapshot(BridgeTopic::SettingsTranscription);
    assert_eq!(
        transcription["error"],
        "The Neural Engine could not be turned back on yet. Try again, or quit and reopen Steno."
    );
    assert_eq!(
        transcription["neuralEngineOff"], true,
        "the offer stays while this run's pipeline is still on the sidecar"
    );
    // A refresh of the rows (here a remove's) keeps it too.
    harness
        .host
        .settings_transcription_remove(AssetIdParams {
            asset_id: "offlineDiarizer".to_owned(),
        })
        .unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsTranscription)["neuralEngineOff"],
        true
    );

    harness.fakes.pipeline.fail_calls(None);
    harness
        .host
        .settings_transcription_use_neural_engine()
        .unwrap();
    assert_eq!(
        *harness.fakes.pipeline.reloads.lock().unwrap(),
        2,
        "the second press rebuilds"
    );
    let transcription = harness.snapshot(BridgeTopic::SettingsTranscription);
    assert!(transcription.get("neuralEngineOff").is_none());
    assert!(transcription.get("error").is_none());
}

/// Transcription says so and the pipeline is not rebuilt.
#[test]
fn a_fallback_that_could_not_be_ended_stays_on() {
    let harness = falling_back();
    let models = &harness.fakes.speech_models;
    *models.end_fallback_failure.lock().unwrap() = Some("read-only file system".to_owned());
    harness
        .host
        .settings_transcription_use_neural_engine()
        .unwrap();
    assert_eq!(*harness.fakes.pipeline.reloads.lock().unwrap(), 0);
    let transcription = harness.snapshot(BridgeTopic::SettingsTranscription);
    assert_eq!(transcription["neuralEngineOff"], true);
    assert_eq!(
        transcription["error"],
        "The Neural Engine could not be turned back on."
    );
}

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

/// A harness whose downloads wait at their start until the closure releases them.
fn holding_downloads() -> (Harness, impl Fn()) {
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
    let release = move || {
        let (open, signal) = &*gate;
        *open.lock().unwrap() = true;
        signal.notify_all();
    };
    (harness, release)
}

#[test]
fn the_download_reply_returns_while_the_download_runs() {
    let (harness, release_downloads) = holding_downloads();
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
    release_downloads();
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

/// It stays selected unless the list failed; once back, its own name shows.
#[test]
fn a_chosen_microphone_that_is_not_connected_is_listed_as_not_connected() {
    let harness = Harness::builder().build();
    harness
        .host
        .settings_recording_set_input_device(SetStringParams {
            value: "BuiltInMicrophoneDevice".to_owned(),
        })
        .unwrap();
    let recording = harness.sink.last(BridgeTopic::SettingsRecording).unwrap();
    assert_eq!(recording["inputDeviceUID"], "BuiltInMicrophoneDevice");
    assert_eq!(
        recording["devices"],
        json!([{"name": "Microphone not connected", "uid": "BuiltInMicrophoneDevice"}])
    );
    harness
        .fakes
        .audio_devices
        .fail_listing(Some("PipeWire did not list the devices"));
    harness.host.settings_recording_refresh_devices().unwrap();
    let recording = harness.sink.last(BridgeTopic::SettingsRecording).unwrap();
    assert_eq!(
        recording["devices"],
        json!([]),
        "nothing is known to be connected"
    );
    assert_eq!(recording["error"], "Microphones could not be listed.");
    harness.fakes.audio_devices.fail_listing(None);
    harness
        .fakes
        .audio_devices
        .set_devices(vec![steno_host::services::InputDevice {
            uid: "BuiltInMicrophoneDevice".to_owned(),
            name: "MacBook Pro Microphone".to_owned(),
        }]);
    harness.host.settings_recording_refresh_devices().unwrap();
    let recording = harness.sink.last(BridgeTopic::SettingsRecording).unwrap();
    assert_eq!(
        recording["devices"],
        json!([{"name": "MacBook Pro Microphone", "uid": "BuiltInMicrophoneDevice"}])
    );
    harness
        .host
        .settings_recording_set_input_device(SetStringParams {
            value: String::new(),
        })
        .unwrap();
    harness.fakes.audio_devices.set_devices(Vec::new());
    harness.host.settings_recording_refresh_devices().unwrap();
    assert_eq!(
        harness.sink.last(BridgeTopic::SettingsRecording).unwrap()["devices"],
        json!([]),
        "the system default needs no entry"
    );
}

/// `isTesting` and `isRequesting` go out with the lock released, before the
/// service runs.
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

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

/// A secret store holding `sk-stored` that fails every read, as a locked keyring.
fn a_harness_whose_key_cannot_be_read() -> Harness {
    Harness::builder()
        .seed(|_, fakes| {
            block_on(
                fakes
                    .secrets
                    .set_secret(&SecretKey::llm_api_key(), Some("sk-stored")),
            )
            .unwrap();
            fakes.secrets.fail_reads(Some("the keyring is locked"));
        })
        .build()
}

/// A key the store could not read shows as unreadable, a save of the other
/// fields keeps it, and it shows once the store answers again.
#[test]
fn a_key_that_could_not_be_read_survives_a_save_and_shows_once_read() {
    let harness = a_harness_whose_key_cannot_be_read();
    let summaries = harness.snapshot(BridgeTopic::SettingsSummaries);
    assert_eq!(summaries["error"], KeyRead::UNREADABLE);
    assert_eq!(summaries["errorDetails"], "the keyring is locked");
    assert_eq!(summaries["hasAPIKey"], false);

    harness
        .host
        .settings_summaries_select_preset(SetStringParams {
            value: "openAI".to_owned(),
        })
        .unwrap();
    harness
        .host
        .settings_summaries_update(update(Some("gpt-4.1-mini"), None, None, None))
        .unwrap();
    harness.host.settings_summaries_save().unwrap();
    assert_eq!(
        harness.store.settings().unwrap().llm_model.as_deref(),
        Some("gpt-4.1-mini")
    );
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsSummaries)["error"],
        KeyRead::UNREADABLE,
        "still not read"
    );
    harness.fakes.secrets.fail_reads(None);
    assert_eq!(
        block_on(harness.fakes.secrets.secret(&SecretKey::llm_api_key()))
            .unwrap()
            .as_deref(),
        Some("sk-stored"),
        "the saves left the key alone"
    );

    harness.host.secrets_changed();
    let summaries = harness.snapshot(BridgeTopic::SettingsSummaries);
    assert_eq!(summaries["hasAPIKey"], true);
    assert!(summaries.get("error").is_none(), "{summaries}");
}

/// A field typed and emptied saves nothing over the unreadable key; a key
/// typed, before or after the store answered, is saved.
#[test]
fn only_a_typed_key_replaces_one_that_could_not_be_read() {
    let harness = a_harness_whose_key_cannot_be_read();
    let stored = || {
        harness.fakes.secrets.fail_reads(None);
        let key = block_on(harness.fakes.secrets.secret(&SecretKey::llm_api_key())).unwrap();
        harness
            .fakes
            .secrets
            .fail_reads(Some("the keyring is locked"));
        key
    };
    harness
        .host
        .settings_summaries_select_preset(SetStringParams {
            value: "openAI".to_owned(),
        })
        .unwrap();
    for typed in ["s", ""] {
        harness
            .host
            .settings_summaries_update(update(None, None, Some(typed), None))
            .unwrap();
    }
    harness
        .host
        .settings_summaries_update(update(Some("gpt-4.1-mini"), None, None, None))
        .unwrap();
    harness.host.settings_summaries_save().unwrap();
    assert_eq!(stored().as_deref(), Some("sk-stored"), "typed and erased");

    // Typed, then the store answers again before a save.
    harness
        .host
        .settings_summaries_update(update(None, None, Some("sk-half"), None))
        .unwrap();
    harness.fakes.secrets.fail_reads(None);
    harness.host.secrets_changed();
    harness.host.settings_summaries_save().unwrap();
    assert_eq!(
        stored().as_deref(),
        Some("sk-half"),
        "the reread kept the edit"
    );

    harness
        .host
        .settings_summaries_update(update(None, None, Some("sk-typed"), None))
        .unwrap();
    harness.host.settings_summaries_save().unwrap();
    assert_eq!(stored().as_deref(), Some("sk-typed"));
    let summaries = harness.snapshot(BridgeTopic::SettingsSummaries);
    assert!(summaries.get("error").is_none(), "{summaries}");
}

#[test]
fn a_reread_key_keeps_the_forms_unsaved_edits() {
    let harness = a_harness_whose_key_cannot_be_read();
    harness
        .host
        .settings_summaries_select_preset(SetStringParams {
            value: "openAI".to_owned(),
        })
        .unwrap();
    harness
        .host
        .settings_summaries_update(update(Some("gpt-typed"), None, None, None))
        .unwrap();
    harness.fakes.secrets.fail_reads(None);
    harness.host.secrets_changed();
    let summaries = harness.snapshot(BridgeTopic::SettingsSummaries);
    assert_eq!(summaries["hasAPIKey"], true);
    assert_eq!(summaries["model"], "gpt-typed");
    assert!(summaries.get("error").is_none(), "{summaries}");
    harness.host.settings_summaries_save().unwrap();
    assert_eq!(
        harness.store.settings().unwrap().llm_model.as_deref(),
        Some("gpt-typed")
    );
    assert_eq!(
        block_on(harness.fakes.secrets.secret(&SecretKey::llm_api_key()))
            .unwrap()
            .as_deref(),
        Some("sk-stored")
    );
}

/// A save that leaves the field alone keeps it.
#[test]
fn emptying_the_key_field_removes_the_stored_key() {
    let harness = Harness::builder()
        .seed(|_, fakes| {
            block_on(
                fakes
                    .secrets
                    .set_secret(&SecretKey::llm_api_key(), Some("sk-stored")),
            )
            .unwrap();
        })
        .build();
    let stored = || block_on(harness.fakes.secrets.secret(&SecretKey::llm_api_key())).unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsSummaries)["hasAPIKey"],
        true
    );
    harness
        .host
        .settings_summaries_select_preset(SetStringParams {
            value: "openAI".to_owned(),
        })
        .unwrap();
    harness
        .host
        .settings_summaries_update(update(Some("gpt-4.1-mini"), None, None, None))
        .unwrap();
    harness.host.settings_summaries_save().unwrap();
    assert_eq!(stored().as_deref(), Some("sk-stored"));

    harness
        .host
        .settings_summaries_update(update(None, None, Some(""), None))
        .unwrap();
    harness.host.settings_summaries_save().unwrap();
    assert_eq!(stored(), None);
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsSummaries)["hasAPIKey"],
        false
    );
}

/// Invalid input saves nothing; a commit saves only changes and then probes.
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
    let stored_key = block_on(harness.fakes.secrets.secret(&SecretKey::llm_api_key())).unwrap();
    assert_eq!(
        stored_key.as_deref(),
        Some("sk-test"),
        "the key goes to the secret store, trimmed"
    );
    let rendered =
        steno_core::json::to_canonical_string(&harness.snapshot(BridgeTopic::SettingsSummaries))
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

/// Closing the consent card stores nothing, and the Codex configuration
/// keeps the endpoint fields.
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

/// While the vault check is held, other calls reach the host within [`LIVE`]
/// and the section reads unsaved; once it answers, the vault is stored.
#[test]
fn a_held_vault_check_leaves_the_host_live() {
    let harness = Harness::builder()
        .choose(Some("/Users/nicolai/Notes/Work Vault"))
        .build();
    harness
        .host
        .settings_export_set_enabled(SetBoolParams { value: true })
        .unwrap();
    let (checking, release) = hold_vault_checks(&harness);
    let saving = std::thread::spawn({
        let host = harness.host.clone();
        move || host.settings_export_choose_vault()
    });
    checking
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the vault check began");
    let since = std::time::Instant::now();
    harness.host.recorder_changed();
    let levels = since.elapsed();
    let since = std::time::Instant::now();
    harness.host.settings_recording_refresh_devices().unwrap();
    let command = since.elapsed();
    let during = harness.snapshot(BridgeTopic::SettingsExport);
    release.send(()).unwrap();
    saving.join().unwrap().unwrap();

    assert!(levels < LIVE, "the level hook took {levels:?}");
    assert!(command < LIVE, "the Settings command took {command:?}");
    assert_eq!(during["saved"], false);
    assert_eq!(
        harness
            .store
            .settings()
            .unwrap()
            .obsidian
            .unwrap()
            .vault_path,
        "/Users/nicolai/Notes/Work Vault"
    );
    assert_eq!(harness.snapshot(BridgeTopic::SettingsExport)["saved"], true);
}

/// The destination's message shows verbatim, and disabling saves without
/// validating.
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

/// An Obsidian value with a field this build does not know.
const FUTURE_OBSIDIAN: &str =
    r#"{"futureFolder":"Daily","includeAudio":false,"vaultPath":"/vault"}"#;

fn put_setting_row(store: &steno_core::Store, key: &str, value: &str) {
    store
        .write(|transaction| {
            transaction.execute(
                "INSERT OR REPLACE INTO setting (key, value) VALUES (?1, ?2)",
                [key, value],
            )?;
            Ok(())
        })
        .unwrap();
}

fn setting_row(harness: &Harness, key: &str) -> Option<String> {
    harness
        .store
        .read(|connection| {
            let mut statement = connection.prepare("SELECT value FROM setting WHERE key = ?1")?;
            let mut rows = statement.query([key])?;
            Ok(match rows.next()? {
                Some(row) => Some(row.get(0)?),
                None => None,
            })
        })
        .unwrap()
}

#[test]
fn an_export_edit_keeps_unknown_rows_and_obsidian_fields() {
    let harness = Harness::builder()
        .seed(|store, _| {
            put_setting_row(store, "obsidian", FUTURE_OBSIDIAN);
            put_setting_row(store, "aFutureSetting", "7");
        })
        .build();
    harness
        .host
        .settings_export_update(ExportUpdateParams {
            people_folder: None,
            include_audio: Some(true),
            task_tag: None,
        })
        .unwrap();
    assert_eq!(
        setting_row(&harness, "obsidian").as_deref(),
        Some(r#"{"futureFolder":"Daily","includeAudio":true,"vaultPath":"/vault"}"#)
    );

    // A field another writer added after the host loaded is kept too.
    put_setting_row(
        &harness.store,
        "obsidian",
        r#"{"futureFolder":"Daily","includeAudio":true,"laterField":1,"vaultPath":"/vault"}"#,
    );
    harness
        .host
        .settings_export_update(ExportUpdateParams {
            people_folder: None,
            include_audio: None,
            task_tag: Some("todo".to_owned()),
        })
        .unwrap();
    harness.host.settings_export_save().unwrap();
    assert_eq!(
        setting_row(&harness, "obsidian").as_deref(),
        Some(
            r#"{"futureFolder":"Daily","includeAudio":true,"laterField":1,"taskTag":"todo","vaultPath":"/vault"}"#
        )
    );
    assert_eq!(
        setting_row(&harness, "aFutureSetting").as_deref(),
        Some("7")
    );
    assert!(
        !harness
            .snapshot(BridgeTopic::SettingsExport)
            .to_string()
            .contains("futureFolder"),
        "the bridge shows only the fields it knows"
    );
}

/// The stored export settings the undo tests start from.
const STORED_OBSIDIAN: &str = r#"{"includeAudio":false,"vaultPath":"/Vaults/Stored"}"#;

fn include_audio(host: &steno_host::Host, on: bool) {
    host.settings_export_update(ExportUpdateParams {
        people_folder: None,
        include_audio: Some(on),
        task_tag: None,
    })
    .unwrap();
}

#[test]
fn after_a_refused_check_a_store_change_reloads_the_form() {
    let harness = Harness::builder()
        .seed(|store, _| put_setting_row(store, "obsidian", STORED_OBSIDIAN))
        .build();
    harness
        .fakes
        .export_validator
        .fail_validation(Some("The Obsidian vault at /Vaults/Stored does not exist."));
    include_audio(&harness.host, true);
    assert!(
        !harness
            .store
            .settings()
            .unwrap()
            .obsidian
            .unwrap()
            .include_audio,
        "the refused save stored nothing"
    );
    put_setting_row(
        &harness.store,
        "obsidian",
        r#"{"includeAudio":false,"peopleFolder":"Team","vaultPath":"/Vaults/Stored"}"#,
    );
    harness.host.store_changed();
    let export = harness.snapshot(BridgeTopic::SettingsExport);
    assert_eq!(export["peopleFolder"], "Team");
    assert_eq!(export["includeAudio"], false);
}

/// The unknown fields may belong to the vault.
#[test]
fn another_vault_starts_without_the_unknown_obsidian_fields() {
    let harness = Harness::builder()
        .seed(|store, _| put_setting_row(store, "obsidian", FUTURE_OBSIDIAN))
        .choose(Some("/other"))
        .build();
    harness.host.settings_export_choose_vault().unwrap();
    assert_eq!(
        setting_row(&harness, "obsidian").as_deref(),
        Some(r#"{"includeAudio":false,"vaultPath":"/other"}"#)
    );
}

/// The draft carries the stored unknown fields, so it equals what is stored.
#[test]
fn a_commit_without_an_edit_saves_nothing_when_unknown_fields_are_stored() {
    let harness = Harness::builder()
        .seed(|store, _| put_setting_row(store, "obsidian", FUTURE_OBSIDIAN))
        .build();
    harness
        .host
        .settings_export_set_enabled(SetBoolParams { value: true })
        .unwrap();
    harness.host.settings_export_save().unwrap();
    assert!(
        harness
            .fakes
            .export_validator
            .validated
            .lock()
            .unwrap()
            .is_empty(),
        "nothing was validated, so nothing was saved"
    );
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsExport)["saved"],
        false
    );
}

/// The iPhone section and the sidebar say why; Try again runs busy with the
/// host's lock released.
#[test]
fn phones_waiting_try_again_until_the_pairing_reads() {
    let harness = Harness::builder()
        .with_handover("steno-mac-7f3a", 52_431)
        .seed(|_, fakes| {
            let handover = fakes.handover.as_ref().unwrap();
            handover.pair(paired_phone());
            *handover.waiting.lock().unwrap() = Some(PhoneWaitingReason::Unreadable);
            *handover.try_again_failure.lock().unwrap() = Some("the keychain is locked".to_owned());
        })
        .build();
    let handover = harness.fakes.handover.clone().unwrap();
    let phone = harness.snapshot(BridgeTopic::SettingsPhone);
    assert_eq!(
        phone["waiting"],
        json!({
            "reason": "unreadable",
            "message": "Steno could not read this Mac's phone pairing. Choose Try again, then Always Allow when macOS asks.",
            "busy": false,
        })
    );
    assert_eq!(phone["subtitle"], "Phones waiting");
    assert_eq!(phone["devices"][0]["name"], "Nicolai's iPhone");

    harness.sink.clear();
    harness.host.settings_phone_try_again().unwrap();
    let published = harness.sink.all(BridgeTopic::SettingsPhone);
    assert_eq!(
        published[0]["waiting"]["busy"], true,
        "busy before the call"
    );
    let phone = published.last().unwrap();
    assert_eq!(phone["waiting"]["busy"], false);
    assert_eq!(phone["error"], "Your phones still can't send recordings.");
    assert_eq!(phone["errorDetails"], "the keychain is locked");
    assert_eq!(*handover.tries.lock().unwrap(), 1);

    *handover.try_again_failure.lock().unwrap() = None;
    harness.host.settings_phone_try_again().unwrap();
    let phone = harness.snapshot(BridgeTopic::SettingsPhone);
    assert!(phone.get("waiting").is_none(), "{phone}");
    assert!(phone.get("error").is_none(), "{phone}");
    assert_eq!(phone["subtitle"], "1 iPhone paired");
    assert_eq!(*handover.tries.lock().unwrap(), 2);
}

#[test]
fn the_waiting_reasons_are_plain_sentences() {
    use steno_host::settings::snapshots::waiting_message;
    for reason in PhoneWaitingReason::ALL {
        for platform in Platform::ALL {
            let message = waiting_message(*reason, *platform);
            for word in [
                "identity",
                "secret",
                "fingerprint",
                "keyring entry",
                "listener",
            ] {
                assert!(!message.contains(word), "{reason} on {platform}: {message}");
            }
            // Only the Mac brings a pairing over from the previous version.
            let computer =
                if *platform == Platform::Macos || *reason == PhoneWaitingReason::ImportDenied {
                    "Mac"
                } else {
                    "computer"
                };
            assert!(
                message.contains(computer),
                "{reason} on {platform}: {message}"
            );
            assert_eq!(
                message.contains("keyring"),
                *platform == Platform::Linux && *reason == PhoneWaitingReason::Unreadable,
                "{reason} on {platform}: {message}"
            );
        }
    }
    // Pair again cannot help with a file or the clock.
    for platform in Platform::ALL {
        for reason in [
            PhoneWaitingReason::FileNotOpened,
            PhoneWaitingReason::ClockBehind,
        ] {
            let message = waiting_message(reason, *platform);
            assert!(!message.contains("Pair again"), "{platform}: {message}");
        }
    }
}

/// Pair again asks first while the section shows busy: No changes nothing,
/// Yes removes the phones and ends the wait; a failure says so.
#[test]
fn phones_waiting_pair_again_after_the_warning() {
    let answer = Arc::new(Mutex::new(false));
    let answered = answer.clone();
    let asks = Arc::new(Mutex::new(0));
    let asked = asks.clone();
    let harness = Harness::builder()
        .with_handover("steno-mac-7f3a", 52_431)
        .confirm_with(move |host, prompt| {
            *asked.lock().unwrap() += 1;
            assert_eq!(
                *prompt,
                steno_host::host::pair_again_prompt(Platform::Macos)
            );
            assert!(prompt.message.contains("Each phone must scan a new code"));
            let phone = host.snapshot(BridgeTopic::SettingsPhone).unwrap();
            assert_eq!(phone["waiting"]["busy"], true, "busy while asked");
            *answered.lock().unwrap()
        })
        .seed(|_, fakes| {
            let handover = fakes.handover.as_ref().unwrap();
            handover.pair(paired_phone());
            *handover.waiting.lock().unwrap() = Some(PhoneWaitingReason::Missing);
        })
        .build();
    let handover = harness.fakes.handover.clone().unwrap();

    let reply = harness.host.settings_phone_pair_again().unwrap();
    assert!(!reply.confirmed);
    assert_eq!(*handover.pairings_again.lock().unwrap(), 0);
    let phone = harness.snapshot(BridgeTopic::SettingsPhone);
    assert_eq!(phone["waiting"]["busy"], false);
    assert_eq!(phone["devices"].as_array().unwrap().len(), 1);

    *answer.lock().unwrap() = true;
    *handover.pair_again_failure.lock().unwrap() = Some("the keychain refused the write".into());
    assert!(harness.host.settings_phone_pair_again().unwrap().confirmed);
    let phone = harness.snapshot(BridgeTopic::SettingsPhone);
    assert_eq!(phone["error"], "Steno could not finish the new pairing.");
    assert_eq!(phone["errorDetails"], "the keychain refused the write");
    assert_eq!(phone["waiting"]["reason"], "missing");

    *handover.pair_again_failure.lock().unwrap() = None;
    assert!(harness.host.settings_phone_pair_again().unwrap().confirmed);
    assert_eq!(*handover.pairings_again.lock().unwrap(), 2);
    let phone = harness.snapshot(BridgeTopic::SettingsPhone);
    assert!(phone.get("waiting").is_none(), "{phone}");
    assert!(phone.get("error").is_none(), "{phone}");
    assert_eq!(phone["devices"], json!([]));
    assert_eq!(phone["subtitle"], "No iPhone paired");

    // A page from before the wait ended asks nothing and runs nothing.
    assert_eq!(*asks.lock().unwrap(), 3);
    assert!(!harness.host.settings_phone_pair_again().unwrap().confirmed);
    assert_eq!(
        *asks.lock().unwrap(),
        3,
        "no question once it no longer waits"
    );
    assert_eq!(*handover.pairings_again.lock().unwrap(), 2);
}

/// Without a handover service the section is unavailable.
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
    harness.host.reload_pass();
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

    // Cancel while the code is up: the code and the listener go.
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
    harness.host.reload_pass();
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

/// A failed rebuild, then a reload that succeeds from another section: the
/// offer goes and a waiting meeting no longer says the Neural Engine is off.
#[test]
fn a_later_successful_reload_ends_a_failed_rebuilds_offer() {
    let harness = Harness::builder()
        .seed(|store, fakes| {
            let mut meeting = sample_meeting();
            meeting.id = uuid(0x8D);
            meeting.state = MeetingState::Queued;
            meeting.summary = None;
            store.save_meeting(&meeting).unwrap();
            fakes
                .speech_models
                .speech_fallback
                .store(true, Ordering::SeqCst);
        })
        .build();
    let title =
        |harness: &Harness| harness.snapshot(BridgeTopic::Progress)["entries"][0]["title"].clone();
    harness
        .host
        .apply_meeting_event(&MeetingEvent::ModelsMissing {
            meeting_id: uuid(0x8D),
        });
    harness.fakes.pipeline.fail_calls(Some("the store is busy"));
    harness
        .host
        .settings_transcription_use_neural_engine()
        .unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsTranscription)["neuralEngineOff"],
        true
    );
    assert_eq!(
        title(&harness),
        "Neural Engine off after a crash: download the speech models in Settings"
    );
    harness.fakes.pipeline.fail_calls(None);
    harness
        .host
        .settings_summaries_update(update(
            Some("a-model"),
            Some("http://localhost:1/v1"),
            None,
            None,
        ))
        .unwrap();
    harness.host.settings_summaries_save().unwrap();
    assert_eq!(
        *harness.fakes.pipeline.reloads.lock().unwrap(),
        2,
        "the save rebuilt the pipeline"
    );
    assert!(
        harness
            .snapshot(BridgeTopic::SettingsTranscription)
            .get("neuralEngineOff")
            .is_none(),
        "the pipeline is on the Neural Engine again"
    );
    assert_eq!(title(&harness), "Download the speech models in Settings");
}
