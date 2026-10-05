//! The host on Windows and Linux: the permissions it lists and the
//! sentences that name the machine. The other suites run the Mac, whose
//! words are Swift's; these pass `Platform::Windows` or `Platform::Linux`
//! to the harness and check what changes, so every OS's CI checks every
//! OS's words.

mod common;

use steno_host::services::Preferences as _;

use std::sync::{Arc, Mutex};

use common::*;
use serde_json::{Value, json};
use steno_bridge::{
    BridgeErrorCode, BridgeHost, BridgeTopic, MeetingIdParams, PermissionKind, PermissionState,
    Platform,
};
use steno_host::onboarding::OnboardingViewModel;
use steno_host::services::FileSystem as _;

fn kinds(permissions: &Value) -> Vec<&str> {
    permissions
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["kind"].as_str().unwrap())
        .collect()
}

#[test]
fn onboarding_and_recording_list_the_permissions_the_platform_has() {
    for (platform, onboarding, recording) in [
        (
            Platform::Macos,
            vec!["microphone", "systemAudio", "calendar", "localNetwork"],
            vec!["microphone", "systemAudio"],
        ),
        (
            Platform::Windows,
            vec!["microphone", "localNetwork"],
            vec!["microphone"],
        ),
        (Platform::Linux, vec!["microphone"], vec!["microphone"]),
    ] {
        let harness = Harness::builder().platform(platform).build();
        assert_eq!(
            kinds(&harness.snapshot(BridgeTopic::Onboarding)["permissions"]),
            onboarding,
            "{platform}"
        );
        assert_eq!(
            kinds(&harness.snapshot(BridgeTopic::SettingsRecording)["permissions"]),
            recording,
            "{platform}"
        );
    }
}

/// A denied system audio tap counts on the Mac only: Linux records the
/// sink without a permission, so its onboarding is done and Recording is
/// ready with the microphone alone.
#[test]
fn only_the_platforms_own_permissions_gate_onboarding_and_recording() {
    let deny_system_audio = |_: &steno_core::Store, fakes: &steno_host::fakes::FakeServices| {
        fakes
            .permissions
            .set_state(PermissionKind::SystemAudio, PermissionState::Denied);
        fakes
            .preferences
            .set_flag(OnboardingViewModel::COMPLETED_KEY, true);
    };
    let mac = Harness::builder().seed(deny_system_audio).build();
    assert!(mac.host.should_open_onboarding());
    assert_eq!(
        mac.snapshot(BridgeTopic::Onboarding)["permissionsComplete"],
        false
    );
    assert_eq!(
        mac.snapshot(BridgeTopic::SettingsRecording)["subtitle"],
        "Permission needed"
    );

    let linux = Harness::builder()
        .platform(Platform::Linux)
        .seed(deny_system_audio)
        .build();
    assert!(!linux.host.should_open_onboarding());
    assert_eq!(
        linux.snapshot(BridgeTopic::Onboarding)["permissionsComplete"],
        true
    );
    assert_eq!(
        linux.snapshot(BridgeTopic::SettingsRecording)["subtitle"],
        "Ready"
    );
}

#[test]
fn the_local_presets_run_on_this_computer_off_the_mac() {
    let titles = |platform| {
        let harness = Harness::builder().platform(platform).build();
        let presets = harness.snapshot(BridgeTopic::SettingsSummaries)["presets"].clone();
        presets
            .as_array()
            .unwrap()
            .iter()
            .filter(|preset| preset["showsServerField"] == json!(true))
            .map(|preset| preset["title"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        titles(Platform::Macos)[..2],
        ["LM Studio on this Mac", "Ollama on this Mac"]
    );
    assert_eq!(
        titles(Platform::Windows)[..2],
        ["LM Studio on this computer", "Ollama on this computer"]
    );
}

#[test]
fn the_banner_and_the_delete_prompt_name_this_computer() {
    let asked = Arc::new(Mutex::new(None::<String>));
    let seen = asked.clone();
    let harness = Harness::builder()
        .platform(Platform::Linux)
        .confirm_with(move |_, params| {
            *seen.lock().unwrap() = Some(params.message.clone());
            false
        })
        .seed(populate_sample)
        .build();
    // The banner needs the list to have filled once.
    let _ = harness.snapshot(BridgeTopic::MeetingsList);
    assert_eq!(
        harness.snapshot(BridgeTopic::App)["setupBanner"]["body"],
        "Steno has no LLM endpoint and no Obsidian vault yet, so meetings keep a raw transcript on this computer."
    );
    harness
        .host
        .meetings_delete(MeetingIdParams {
            meeting_id: uuid(MEETING),
        })
        .unwrap();
    assert_eq!(
        asked.lock().unwrap().as_deref(),
        Some(
            "The transcript, summary, tasks and the recording on this computer are removed. Files already exported to Obsidian stay. People stay."
        )
    );
}

#[test]
fn a_recording_that_is_gone_is_no_longer_on_this_computer() {
    let harness = Harness::builder()
        .platform(Platform::Windows)
        .seed(populate_sample)
        .build();
    harness
        .fakes
        .file_system
        .remove(&master_path(&harness.audio_folder()))
        .unwrap();
    harness.host.store_changed();
    let gone = harness.host.meeting_reveal_recording().unwrap_err();
    assert_eq!(gone.code, BridgeErrorCode::NotFound);
    assert_eq!(gone.message, "The recording is no longer on this computer.");
}

/// Linux's one step is current while it is open and stays current once
/// granted: the last step, never a step the platform does not have.
#[test]
fn the_current_step_on_linux_is_always_the_microphone() {
    let mut model = OnboardingViewModel::new(Platform::Linux);
    assert_eq!(model.current(), PermissionKind::Microphone);
    model.finish_request(PermissionKind::Microphone, PermissionState::Granted);
    assert!(model.permissions_handled());
    assert_eq!(model.current(), PermissionKind::Microphone);
}
