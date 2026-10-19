//! Onboarding through the host and the view model.

// Some tests are long.
#![allow(clippy::too_many_lines)]

mod common;

use steno_host::services::Preferences as _;

use common::*;
use serde_json::json;
use steno_bridge::{
    BridgeErrorCode, BridgeHost, BridgeTopic, BridgeWindow, OnboardingSetupStepKind,
    PermissionKind, PermissionKindParams, PermissionState, Platform, SetStringParams,
    SetupStepParams, SummariesUpdateParams, WindowParams,
};
use steno_core::AudioRetention;
use steno_host::onboarding::OnboardingViewModel;
use steno_host::speech::ModelAsset;

/// A harness with the speech models installed.
fn builder() -> HarnessBuilder {
    Harness::builder().seed(|_, fakes| install_speech_models(fakes))
}

fn permissions(
    mic: PermissionState,
    system: PermissionState,
    calendar: PermissionState,
) -> impl FnOnce(&steno_core::Store, &steno_host::fakes::FakeServices) + 'static {
    move |_, fakes| {
        fakes.permissions.set_state(PermissionKind::Microphone, mic);
        fakes
            .permissions
            .set_state(PermissionKind::SystemAudio, system);
        fakes
            .permissions
            .set_state(PermissionKind::Calendar, calendar);
        fakes
            .permissions
            .set_state(PermissionKind::LocalNetwork, PermissionState::Unknown);
    }
}

#[test]
fn the_retention_sentence_follows_the_stored_rule() {
    let harness = builder().build();
    let sentence = harness.snapshot(BridgeTopic::Onboarding)["retentionSentence"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        sentence,
        "Recordings are kept until you delete them. Change this any time in Settings > Recording."
    );
    set_retention(&harness.store, AudioRetention::KeepDays(7));
    harness.host.onboarding_refresh().unwrap();
    let sentence = harness.snapshot(BridgeTopic::Onboarding)["retentionSentence"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        sentence.contains("deleted 7 days after it was processed and exported"),
        "{sentence}"
    );
    assert!(sentence.ends_with("Change this any time in Settings > Recording."));
    set_retention(&harness.store, AudioRetention::DeleteAfterProcessing);
    harness.host.onboarding_refresh().unwrap();
    assert!(
        harness.snapshot(BridgeTopic::Onboarding)["retentionSentence"]
            .as_str()
            .unwrap()
            .starts_with("Each recording is deleted as soon as")
    );
}

/// A request updates only its step; page ready and the permission commands
/// republish.
#[test]
fn steps_run_in_order_required_ones_gate_completion_and_optional_ones_can_be_skipped() {
    let harness = builder()
        .seed(permissions(
            PermissionState::Unknown,
            PermissionState::Unknown,
            PermissionState::Unknown,
        ))
        .build();
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(onboarding["page"], "permissions");
    assert_eq!(onboarding["permissionsComplete"], false);
    let kinds: Vec<&str> = onboarding["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        ["microphone", "systemAudio", "calendar", "localNetwork"]
    );
    assert_eq!(onboarding["permissions"][0]["isRequired"], true);
    assert_eq!(onboarding["permissions"][2]["isRequired"], false);
    assert!(
        onboarding.get("summaries").is_none(),
        "page 1 carries no form"
    );

    harness
        .host
        .onboarding_skip(PermissionKindParams {
            kind: PermissionKind::Microphone,
        })
        .unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["permissions"][0]["isSkipped"],
        false,
        "required steps cannot be skipped"
    );

    harness.sink.clear();
    harness
        .host
        .onboarding_request(PermissionKindParams {
            kind: PermissionKind::Microphone,
        })
        .unwrap();
    let published = harness.sink.all(BridgeTopic::Onboarding);
    assert_eq!(
        published[0]["permissions"][0]["isRequesting"], true,
        "the prompt is up"
    );
    let after = published.last().unwrap();
    assert_eq!(after["permissions"][0]["state"], "granted");
    assert_eq!(after["permissions"][0]["isRequesting"], false);
    assert_eq!(
        after["permissions"][1]["state"], "unknown",
        "only the requested step changes"
    );
    assert_eq!(after["permissionsComplete"], false);

    harness
        .fakes
        .permissions
        .set_answer(PermissionKind::SystemAudio, PermissionState::Denied);
    harness
        .host
        .onboarding_request(PermissionKindParams {
            kind: PermissionKind::SystemAudio,
        })
        .unwrap();
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(onboarding["permissions"][1]["state"], "denied");
    assert_eq!(
        onboarding["page"], "permissions",
        "a denied required step stays"
    );
    harness
        .host
        .system_open_system_settings(PermissionKindParams {
            kind: PermissionKind::SystemAudio,
        })
        .unwrap();
    assert_eq!(
        *harness.fakes.permissions.opened_panes.lock().unwrap(),
        vec![PermissionKind::SystemAudio]
    );

    // The user fixed it in System Settings: "Check again" reads the states.
    harness
        .fakes
        .permissions
        .set_state(PermissionKind::SystemAudio, PermissionState::Granted);
    harness.host.onboarding_refresh().unwrap();
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(onboarding["permissionsComplete"], true);
    assert_eq!(
        onboarding["page"], "permissions",
        "optional steps are still open"
    );

    harness
        .host
        .onboarding_skip(PermissionKindParams {
            kind: PermissionKind::Calendar,
        })
        .unwrap();
    harness
        .host
        .onboarding_skip(PermissionKindParams {
            kind: PermissionKind::LocalNetwork,
        })
        .unwrap();
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(
        onboarding["page"], "setup",
        "page 1 moves on by itself once every step is handled"
    );
    assert_eq!(onboarding["permissions"][3]["isSkipped"], true);
    assert!(onboarding.get("summaries").is_some());
    assert_eq!(onboarding["vault"], json!({}));

    harness.host.onboarding_back().unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["page"],
        "permissions",
        "Back stays on page 1"
    );
    harness.host.onboarding_advance().unwrap();
    assert_eq!(harness.snapshot(BridgeTopic::Onboarding)["page"], "setup");
}

/// Finishing with rows open or after a cancelled chooser sets the flag.
#[test]
fn granted_permissions_open_on_the_setup_page_and_handled_rows_finish() {
    let harness = builder().choose(None).build();
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(onboarding["page"], "setup", "every permission granted");
    assert_eq!(onboarding["finished"], false);
    assert_eq!(
        onboarding["setup"],
        json!([{"kind": "summaries", "state": "open"}, {"kind": "vault", "state": "open"}])
    );

    let reply = harness.host.onboarding_choose_vault().unwrap();
    assert_eq!(reply.path, None);
    assert_eq!(
        harness.store.settings().unwrap().obsidian,
        None,
        "a cancelled chooser saves nothing"
    );

    harness
        .host
        .onboarding_skip_setup(SetupStepParams {
            step: OnboardingSetupStepKind::Summaries,
        })
        .unwrap();
    assert_eq!(harness.snapshot(BridgeTopic::Onboarding)["finished"], false);
    harness
        .host
        .onboarding_skip_setup(SetupStepParams {
            step: OnboardingSetupStepKind::Vault,
        })
        .unwrap();
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(onboarding["finished"], true, "both rows handled finishes");
    assert_eq!(onboarding["setup"][1]["state"], "skipped");
    assert!(
        harness
            .fakes
            .preferences
            .flag(OnboardingViewModel::COMPLETED_KEY)
    );
    // Finishing closes the window from the host, after the page saw `finished`.
    assert_eq!(
        *harness.fakes.opener.windows_closed.lock().unwrap(),
        vec![BridgeWindow::Onboarding]
    );
    assert_eq!(
        harness.sink.last(BridgeTopic::Onboarding).unwrap()["finished"],
        true
    );

    harness
        .host
        .window_close(WindowParams {
            window: BridgeWindow::Onboarding,
            section: None,
            meeting_id: None,
        })
        .unwrap();
    assert_eq!(
        harness.fakes.opener.windows_closed.lock().unwrap().len(),
        2,
        "the page's own close still works"
    );
    let error = harness
        .host
        .window_close(WindowParams {
            window: BridgeWindow::Settings,
            section: None,
            meeting_id: None,
        })
        .unwrap_err();
    assert_eq!(error.code, BridgeErrorCode::InvalidParams);

    let open_rows = builder().build();
    open_rows.host.onboarding_finish().unwrap();
    assert_eq!(
        open_rows.snapshot(BridgeTopic::Onboarding)["finished"],
        true,
        "Finish with rows open sets the flag"
    );
    assert!(
        open_rows
            .fakes
            .preferences
            .flag(OnboardingViewModel::COMPLETED_KEY)
    );
    assert_eq!(
        *open_rows.fakes.opener.windows_closed.lock().unwrap(),
        vec![BridgeWindow::Onboarding]
    );
    open_rows.host.onboarding_back().unwrap();
    assert_eq!(
        open_rows.fakes.opener.windows_closed.lock().unwrap().len(),
        1,
        "a later command does not close it again"
    );
    // The window closed; one opened again starts over on the setup page,
    // and its Finish closes it again.
    open_rows.host.onboarding_window_closed();
    let reopened = open_rows.sink.last(BridgeTopic::Onboarding).unwrap();
    assert_eq!(reopened["finished"], false);
    open_rows.host.onboarding_finish().unwrap();
    assert_eq!(
        *open_rows.fakes.opener.windows_closed.lock().unwrap(),
        vec![BridgeWindow::Onboarding; 2]
    );

    // The window's own close button: the flag alone, no further rule.
    let closed = builder().build();
    assert!(
        !closed
            .fakes
            .preferences
            .flag(OnboardingViewModel::COMPLETED_KEY)
    );
    closed.host.onboarding_window_closed();
    assert!(
        closed
            .fakes
            .preferences
            .flag(OnboardingViewModel::COMPLETED_KEY)
    );
    assert_eq!(closed.snapshot(BridgeTopic::Onboarding)["finished"], false);
}

/// The Summaries form's commands answer on the calling window's model.
#[test]
fn the_summaries_form_commands_answer_on_the_calling_windows_model() {
    let harness = builder()
        .seed(|_, fakes| {
            fakes
                .llm
                .set_codex_account(Ok("nicolai@example.com (Plus)"));
        })
        .build();
    let onboarding = harness.host.for_window(BridgeWindow::Onboarding);
    assert_eq!(onboarding.window(), BridgeWindow::Onboarding);
    assert_eq!(harness.host.window(), BridgeWindow::Main);
    // A draft typed in Settings must not be what onboarding commits.
    harness
        .host
        .settings_summaries_update(update(Some("settings-draft"), None))
        .unwrap();

    // selectPreset: the radio moves on page 2 only.
    harness.sink.clear();
    onboarding
        .settings_summaries_select_preset(SetStringParams {
            value: "openAI".to_owned(),
        })
        .unwrap();
    let page = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(page["summaries"]["presetID"], "openAI");
    assert_eq!(page["summaries"]["baseURL"], "https://api.openai.com/v1");
    // The Settings section follows the store once another window wrote it:
    // its unsaved draft is gone.
    let summaries = harness.snapshot(BridgeTopic::SettingsSummaries);
    assert_eq!(summaries["presetID"], "openAI");
    assert_eq!(summaries["model"], "");
    assert_eq!(
        harness.sink.count(BridgeTopic::Onboarding),
        1,
        "the onboarding topic republishes"
    );
    let settings = harness.store.settings().unwrap();
    assert_eq!(
        settings.llm_base_url.as_deref(),
        Some("https://api.openai.com/v1"),
        "the preset's address is stored, as Swift's selectPreset did"
    );
    assert_eq!(
        settings.llm_model, None,
        "not the Settings window's typed model"
    );

    // update: the draft stays in the onboarding model until its own save.
    onboarding
        .settings_summaries_update(update(Some("gpt-4.1-mini"), Some(" sk-onboarding ")))
        .unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["summaries"]["model"],
        "gpt-4.1-mini"
    );
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsSummaries)["model"],
        "",
        "a page 2 draft is not the Settings draft"
    );
    assert_eq!(harness.store.settings().unwrap().llm_model, None);

    // test: the result shows on page 2, not in Settings.
    onboarding.settings_summaries_test().unwrap();
    let page = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(page["summaries"]["testResult"]["ok"], true);
    assert!(
        harness
            .snapshot(BridgeTopic::SettingsSummaries)
            .get("testResult")
            .is_none()
    );
    assert_eq!(harness.store.settings().unwrap().llm_model, None);

    // refreshCodexStatus: the sign-in lands on the page's card.
    onboarding
        .settings_summaries_select_preset(SetStringParams {
            value: "codex".to_owned(),
        })
        .unwrap();
    harness.fakes.llm.set_codex_account(Err("signed out"));
    onboarding
        .settings_summaries_refresh_codex_status()
        .unwrap();
    let page = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(page["summaries"]["codex"]["signIn"], "unavailable");
    assert_eq!(page["summaries"]["codex"]["signInDetail"], "signed out");
    assert!(
        harness
            .snapshot(BridgeTopic::SettingsSummaries)
            .get("codex")
            .is_none()
    );

    // Back to the endpoint and saved through onboarding's own command.
    onboarding
        .settings_summaries_select_preset(SetStringParams {
            value: "openAI".to_owned(),
        })
        .unwrap();
    harness.host.onboarding_save_summaries().unwrap();
    let settings = harness.store.settings().unwrap();
    assert_eq!(settings.llm_model.as_deref(), Some("gpt-4.1-mini"));
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["setup"][0]["state"],
        "saved"
    );
    // The Settings section follows the store once onboarding wrote it.
    let summaries = harness.snapshot(BridgeTopic::SettingsSummaries);
    assert_eq!(summaries["presetID"], "openAI");
    assert_eq!(summaries["model"], "gpt-4.1-mini");
}

fn update(model: Option<&str>, key: Option<&str>) -> SummariesUpdateParams {
    SummariesUpdateParams {
        base_url: None,
        model: model.map(str::to_owned),
        context_tokens: None,
        api_key: key.map(str::to_owned),
    }
}

/// A vault saved on page 2 is what the Export section saves back.
#[test]
fn a_vault_saved_by_onboarding_survives_the_export_sections_save() {
    let harness = builder()
        .choose(Some("/Users/nicolai/Notes"))
        .seed(|store, _| {
            let mut settings = store.settings().unwrap();
            settings.obsidian = Some(steno_core::ObsidianSettings {
                vault_path: "/Users/nicolai/Old".to_owned(),
                people_folder: None,
                include_audio: false,
                task_tag: None,
                extra: serde_json::Map::new(),
            });
            store.save_settings(&settings).unwrap();
        })
        .build();
    harness.host.onboarding_choose_vault().unwrap();
    assert_eq!(
        harness
            .store
            .settings()
            .unwrap()
            .obsidian
            .unwrap()
            .vault_path,
        "/Users/nicolai/Notes"
    );
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsExport)["vaultName"],
        "Notes",
        "the Export section reloaded"
    );
    harness.host.settings_export_save().unwrap();
    assert_eq!(
        harness
            .store
            .settings()
            .unwrap()
            .obsidian
            .unwrap()
            .vault_path,
        "/Users/nicolai/Notes",
        "the Export section's save keeps the vault onboarding chose"
    );
}

/// The setup commands reach the models and finish.
#[test]
fn saving_summaries_configures_the_endpoint_and_rebuilds_the_pipeline() {
    let harness = builder().choose(Some("/Users/nicolai/Notes")).build();
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["canSaveSummaries"],
        false,
        "no model yet"
    );
    harness.host.onboarding_save_summaries().unwrap();
    assert_eq!(harness.store.settings().unwrap().llm_model, None);

    harness
        .host
        .for_window(BridgeWindow::Onboarding)
        .settings_summaries_update(SummariesUpdateParams {
            base_url: None,
            model: Some("qwen3-8b".to_owned()),
            context_tokens: None,
            api_key: None,
        })
        .unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["canSaveSummaries"],
        true
    );
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsSummaries)["model"],
        "",
        "typed on page 2, not in Settings"
    );
    harness.host.onboarding_save_summaries().unwrap();
    let settings = harness.store.settings().unwrap();
    assert_eq!(settings.llm_model.as_deref(), Some("qwen3-8b"));
    assert_eq!(
        settings.llm_base_url.as_deref(),
        Some("http://127.0.0.1:1234/v1")
    );
    assert_eq!(*harness.fakes.pipeline.reloads.lock().unwrap(), 1);
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(
        onboarding["setup"][0],
        json!({"kind": "summaries", "savedLine": "Saved: qwen3-8b at 127.0.0.1", "state": "saved"})
    );
    assert_eq!(onboarding["summaries"]["isConfigured"], true);
    assert!(
        onboarding["summaries"].get("testResult").is_none(),
        "the onboarding save does not probe"
    );
    assert_eq!(onboarding["finished"], false);

    let reply = harness.host.onboarding_choose_vault().unwrap();
    assert_eq!(reply.path.as_deref(), Some("/Users/nicolai/Notes"));
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(
        onboarding["setup"][1],
        json!({"kind": "vault", "savedLine": "Saved: Notes", "state": "saved"})
    );
    assert_eq!(onboarding["finished"], true, "both rows saved finishes");
    assert_eq!(
        harness
            .store
            .settings()
            .unwrap()
            .obsidian
            .unwrap()
            .vault_path,
        "/Users/nicolai/Notes"
    );
}

#[test]
fn a_refused_vault_after_an_accepted_one_is_not_marked_saved() {
    let harness = builder()
        .choose_in_turn(&["/Vaults/A", "/Vaults/B"])
        .build();
    harness.host.onboarding_choose_vault().unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["setup"][1]["savedLine"],
        "Saved: A"
    );

    harness
        .fakes
        .export_validator
        .fail_validation(Some("The vault at /Vaults/B is not there."));
    harness.host.onboarding_choose_vault().unwrap();
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_ne!(
        onboarding["setup"][1]["savedLine"], "Saved: B",
        "{onboarding}"
    );
    assert_eq!(
        onboarding["vault"]["validationMessage"],
        "The vault at /Vaults/B is not there."
    );
    assert_eq!(
        harness
            .store
            .settings()
            .unwrap()
            .obsidian
            .unwrap()
            .vault_path,
        "/Vaults/A",
        "the store keeps the accepted vault"
    );
}

/// The vault row reads the Obsidian model.
#[test]
fn saving_the_vault_validates_through_the_destination() {
    let harness = builder()
        .choose(Some("/Users/nicolai/Notes/Work Vault"))
        .seed(|_, fakes| {
            fakes.export_validator.fail_validation(Some(
                "The Obsidian vault at /Users/nicolai/Notes/Work Vault does not exist.",
            ));
        })
        .build();
    harness.host.onboarding_choose_vault().unwrap();
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(
        onboarding["vault"],
        json!({"name": "Work Vault", "path": "/Users/nicolai/Notes/Work Vault", "validationMessage": "The Obsidian vault at /Users/nicolai/Notes/Work Vault does not exist."})
    );
    assert_eq!(onboarding["setup"][1]["state"], "open");
    assert_eq!(harness.store.settings().unwrap().obsidian, None);

    // The folder exists now: Save again goes through.
    harness.fakes.export_validator.fail_validation(None);
    harness.host.onboarding_save_vault().unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["setup"][1]["state"],
        "saved"
    );
    assert!(harness.store.settings().unwrap().obsidian.is_some());
}

#[test]
fn the_chatgpt_choice_collapses_the_summaries_row_once_confirmed() {
    let harness = builder()
        .seed(|_, fakes| {
            fakes
                .llm
                .set_codex_account(Ok("nicolai@example.com (Plus)"));
            fakes
                .llm
                .set_codex_models(Ok(vec![steno_host::services::CodexModel {
                    slug: "gpt-5.1-codex".to_owned(),
                    display_name: "GPT-5.1 Codex".to_owned(),
                    context_window: None,
                }]));
        })
        .build();
    harness
        .host
        .onboarding_confirm_summaries_with_codex()
        .unwrap();
    assert_eq!(
        harness.store.settings().unwrap().codex_confirmed_at,
        None,
        "the row's preset is not ChatGPT"
    );

    // The onboarding form's preset switch is the Settings method on the
    // host's Summaries model; the onboarding model follows the store.
    let mut settings = harness.store.settings().unwrap();
    settings.llm_provider = steno_core::LlmProvider::Codex;
    settings.codex_model = Some("gpt-5.1-codex".to_owned());
    settings.codex_confirmed_at = Some(now());
    harness.store.save_settings(&settings).unwrap();
    let stored = builder()
        .seed(|store, fakes| {
            let mut settings = store.settings().unwrap();
            settings.llm_provider = steno_core::LlmProvider::Codex;
            settings.codex_model = Some("gpt-5.1-codex".to_owned());
            settings.codex_confirmed_at = Some(now());
            store.save_settings(&settings).unwrap();
            fakes
                .llm
                .set_codex_account(Ok("nicolai@example.com (Plus)"));
        })
        .build();
    let onboarding = stored.snapshot(BridgeTopic::Onboarding);
    assert_eq!(onboarding["setup"][0]["state"], "saved");
    assert_eq!(
        onboarding["setup"][0]["savedLine"],
        "Saved: gpt-5.1-codex via ChatGPT as nicolai@example.com (Plus)"
    );
    assert_eq!(onboarding["canSaveSummaries"], false, "ChatGPT has no Save");
}

/// A configured install without the flag does not open.
#[test]
fn should_open_follows_the_flag_and_the_required_permissions() {
    let harness = builder().build();
    assert!(harness.host.should_open_onboarding(), "the flag is unset");
    harness
        .fakes
        .preferences
        .set_flag(OnboardingViewModel::COMPLETED_KEY, true);
    assert!(!harness.host.should_open_onboarding());
    harness
        .fakes
        .permissions
        .set_state(PermissionKind::SystemAudio, PermissionState::Denied);
    assert!(
        harness.host.should_open_onboarding(),
        "a missing required permission reopens it"
    );
    harness
        .fakes
        .permissions
        .set_state(PermissionKind::Calendar, PermissionState::Denied);
    harness
        .fakes
        .permissions
        .set_state(PermissionKind::SystemAudio, PermissionState::Granted);
    assert!(
        !harness.host.should_open_onboarding(),
        "optional permissions do not"
    );

    let configured = builder()
        .seed(|store, _| {
            configure_llm(store, "qwen3-8b");
            let mut settings = store.settings().unwrap();
            settings.obsidian = Some(steno_core::ObsidianSettings {
                vault_path: "/vault".to_owned(),
                people_folder: None,
                include_audio: false,
                task_tag: None,
                extra: serde_json::Map::new(),
            });
            store.save_settings(&settings).unwrap();
        })
        .build();
    assert!(
        !configured.host.should_open_onboarding(),
        "nothing left to ask"
    );
    assert!(
        configured
            .fakes
            .preferences
            .flag(OnboardingViewModel::COMPLETED_KEY),
        "and the flag is written"
    );
    let onboarding = configured.snapshot(BridgeTopic::Onboarding);
    assert_eq!(
        onboarding["finished"], true,
        "both rows saved from the store finish at once"
    );
}

/// Denied steps stay current and open Settings; on the model alone.
#[test]
fn current_is_the_first_unhandled_step() {
    let mut model = OnboardingViewModel::new(Platform::Macos);
    assert_eq!(model.current(), PermissionKind::Microphone);
    model.finish_request(PermissionKind::Microphone, PermissionState::Granted);
    assert_eq!(model.current(), PermissionKind::SystemAudio);
    model.finish_request(PermissionKind::SystemAudio, PermissionState::Denied);
    assert_eq!(
        model.current(),
        PermissionKind::SystemAudio,
        "a denied step stays current"
    );
    model.finish_request(PermissionKind::SystemAudio, PermissionState::Granted);
    assert!(model.is_complete());
    assert!(!model.permissions_handled());
    model.skip(PermissionKind::Calendar);
    assert_eq!(model.current(), PermissionKind::LocalNetwork);
    model.skip(PermissionKind::LocalNetwork);
    assert!(model.permissions_handled());
    assert_eq!(
        model.current(),
        PermissionKind::LocalNetwork,
        "the last one when everything is handled"
    );
}

/// It shows the prompts while they are up.
#[test]
fn the_import_step_comes_first_and_moves_on_once_the_identity_came_over() {
    let plain = builder().build();
    assert!(
        plain
            .snapshot(BridgeTopic::Onboarding)
            .get("swiftImport")
            .is_none(),
        "no import, no step"
    );

    let harness = builder()
        .with_swift_import(2)
        .seed(|_, fakes| {
            fakes
                .preferences
                .set_flag(OnboardingViewModel::COMPLETED_KEY, true);
        })
        .build();
    assert!(harness.host.should_open_onboarding(), "the import opens it");
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(onboarding["page"], "import");
    assert_eq!(
        onboarding["swiftImport"],
        json!({"state": "pending", "prompts": 2})
    );

    harness.host.onboarding_import().unwrap();
    let published: Vec<_> = harness
        .sink
        .all(BridgeTopic::Onboarding)
        .iter()
        .map(|snapshot| snapshot["swiftImport"]["state"].clone())
        .collect();
    assert_eq!(
        published.first(),
        Some(&json!("importing")),
        "the page saw the prompts coming"
    );
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(onboarding["swiftImport"]["state"], "done");
    assert_eq!(onboarding["page"], "setup", "every permission granted");
    let import = harness.fakes.swift_import.as_ref().unwrap();
    assert_eq!(*import.runs.lock().unwrap(), 1);
    assert!(
        !harness.host.should_open_onboarding(),
        "nothing left to ask"
    );
}

/// The copied completed flag does not hide grants macOS keeps per bundle id.
#[test]
fn after_the_handoff_a_missing_grant_lands_on_the_permissions_page() {
    let harness = builder()
        .with_swift_import(2)
        .seed(|_, fakes| {
            fakes
                .preferences
                .set_flag(OnboardingViewModel::COMPLETED_KEY, true);
            fakes
                .permissions
                .set_state(PermissionKind::Microphone, PermissionState::Unknown);
            fakes
                .permissions
                .set_state(PermissionKind::SystemAudio, PermissionState::Unknown);
        })
        .build();
    assert!(harness.host.should_open_onboarding());
    harness.host.onboarding_import().unwrap();
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(onboarding["swiftImport"]["state"], "done");
    assert_eq!(onboarding["page"], "permissions");
    assert_eq!(onboarding["permissionsComplete"], false);
    assert!(
        harness.host.should_open_onboarding(),
        "and again at the next launch until each is granted"
    );
}

#[test]
fn a_denied_export_stays_on_the_step_with_try_again_until_the_user_goes_on() {
    let harness = builder().with_swift_import(2).build();
    let import = harness.fakes.swift_import.clone().unwrap();
    import.deny_export("macOS did not let Steno read this Mac's phone pairing.");
    harness.host.onboarding_import().unwrap();
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(onboarding["page"], "import");
    assert_eq!(onboarding["swiftImport"]["state"], "waiting");
    assert_eq!(
        onboarding["swiftImport"]["error"],
        "macOS did not let Steno read this Mac's phone pairing."
    );

    harness.host.onboarding_skip_import().unwrap();
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(onboarding["page"], "setup");
    assert_eq!(onboarding["swiftImport"]["state"], "waiting");
    assert_eq!(*import.skips.lock().unwrap(), 1);
    assert!(
        harness.host.should_open_onboarding(),
        "the step returns while the identity waits"
    );
}

#[test]
fn closing_the_window_over_the_step_skips_it() {
    let harness = builder().with_swift_import(1).build();
    harness.host.onboarding_window_closed();
    let import = harness.fakes.swift_import.as_ref().unwrap();
    assert_eq!(*import.skips.lock().unwrap(), 1);
    assert_eq!(*import.runs.lock().unwrap(), 0);
}

/// Refused in onboarding and Settings alike, run again once idle; a start
/// during the test stops it.
#[test]
fn the_system_audio_test_is_refused_while_recording() {
    use steno_bridge::RecordingState;
    use steno_host::services::RecorderStatus;

    let harness = builder()
        .seed(permissions(
            PermissionState::Unknown,
            PermissionState::Unknown,
            PermissionState::Unknown,
        ))
        .build();
    let system_audio = || PermissionKindParams {
        kind: PermissionKind::SystemAudio,
    };
    for state in [
        RecordingState::Starting,
        RecordingState::Recording,
        RecordingState::Stopping,
    ] {
        harness.fakes.recorder.set_status(RecorderStatus {
            state,
            ..RecorderStatus::idle()
        });
        harness.sink.clear();
        harness.host.onboarding_request(system_audio()).unwrap();
        harness
            .host
            .settings_recording_request_permission(system_audio())
            .unwrap();
        assert_eq!(
            *harness.fakes.permissions.requests.lock().unwrap(),
            Vec::new(),
            "{state:?}: no test ran"
        );
        for topic in [BridgeTopic::Onboarding, BridgeTopic::SettingsRecording] {
            let row = &harness.snapshot(topic)["permissions"][1];
            assert_eq!(row["kind"], "systemAudio");
            assert_eq!(row["state"], "unknown", "{state:?} {topic:?}");
            assert_eq!(row["waitsForRecording"], true, "{state:?} {topic:?}");
            assert!(
                harness
                    .sink
                    .all(topic)
                    .iter()
                    .all(|snapshot| snapshot["permissions"][1]["isRequesting"] == false),
                "{state:?} {topic:?}: never Asking"
            );
        }
    }

    // The recording ends: both rows drop the line before any press.
    harness.fakes.recorder.set_status(RecorderStatus::idle());
    harness.sink.clear();
    harness.host.recorder_changed();
    harness.host.reload_pass();
    for topic in [BridgeTopic::Onboarding, BridgeTopic::SettingsRecording] {
        let published = harness.sink.all(topic);
        let row = &published.last().expect("published")["permissions"][1];
        assert!(row.get("waitsForRecording").is_none(), "{topic:?}: {row}");
    }
    harness.host.onboarding_request(system_audio()).unwrap();
    assert_eq!(
        *harness.fakes.permissions.requests.lock().unwrap(),
        [PermissionKind::SystemAudio]
    );
    let row = &harness.snapshot(BridgeTopic::Onboarding)["permissions"][1];
    assert_eq!(row["state"], "granted");
    assert!(row.get("waitsForRecording").is_none(), "{row}");

    // A start while the test runs: the services stop it.
    harness
        .fakes
        .permissions
        .set_answer(PermissionKind::SystemAudio, PermissionState::Unknown);
    harness.fakes.permissions.set_on_request({
        let recorder = harness.fakes.recorder.clone();
        move |_| {
            recorder.set_status(RecorderStatus {
                state: RecordingState::Starting,
                ..RecorderStatus::idle()
            });
        }
    });
    harness
        .host
        .settings_recording_request_permission(system_audio())
        .unwrap();
    let row = &harness.snapshot(BridgeTopic::SettingsRecording)["permissions"][1];
    assert_eq!(row["state"], "unknown");
    assert_eq!(row["waitsForRecording"], true);
    assert_eq!(row["isRequesting"], false);
}

/// The handoff as the new app finds it: the speech model installed, the
/// speaker models missing, the flag copied, every grant in place.
fn handoff_without_the_diarizer() -> HarnessBuilder {
    Harness::builder().seed(|_, fakes| {
        fakes
            .speech_models
            .set_installed(ModelAsset::ParakeetV3, Some(445_187_200));
        fakes
            .preferences
            .set_flag(OnboardingViewModel::COMPLETED_KEY, true);
    })
}

/// Once onboarding was closed, missing models open it no more.
#[test]
fn missing_speech_models_open_onboarding_on_page_one_until_it_offered_them() {
    let harness = handoff_without_the_diarizer().build();
    assert!(harness.host.should_open_onboarding());
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(onboarding["page"], "permissions");
    assert_eq!(onboarding["permissionsComplete"], true);
    assert_eq!(
        onboarding["speechModels"],
        json!({
            "assetIDs": ["offlineDiarizer"],
            "detail": "Not downloaded · 22 MB",
            "state": "absent",
        })
    );

    harness.host.onboarding_window_closed();
    assert!(
        harness
            .fakes
            .preferences
            .flag(OnboardingViewModel::MODELS_OFFERED_KEY)
    );
    assert!(!harness.host.should_open_onboarding());

    let installed = builder()
        .seed(|_, fakes| {
            fakes
                .preferences
                .set_flag(OnboardingViewModel::COMPLETED_KEY, true);
        })
        .build();
    assert!(!installed.host.should_open_onboarding());
    assert!(
        installed.snapshot(BridgeTopic::Onboarding)["speechModels"].is_null(),
        "no row while every model is installed"
    );
}

#[test]
fn a_fresh_install_offers_every_model_on_one_row() {
    let harness = Harness::builder().build();
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["speechModels"],
        json!({
            "assetIDs": ["parakeetV3", "offlineDiarizer"],
            "detail": "Not downloaded · 507 MB",
            "state": "absent",
        })
    );
}

/// Speech model in, speaker models failed: Retry is sized for those alone.
#[test]
fn retry_after_a_partial_install_starts_only_the_missing_model() {
    let harness = Harness::builder().build();
    let models = harness.fakes.speech_models.clone();
    harness.fakes.speech_models.set_on_download(move |asset| {
        models.fail_downloads((asset == ModelAsset::OfflineDiarizer).then_some("offline"));
    });
    let download = |asset: ModelAsset| {
        harness
            .host
            .for_window(BridgeWindow::Onboarding)
            .settings_transcription_download(steno_bridge::AssetIdParams {
                asset_id: asset.as_str().to_owned(),
            })
            .unwrap();
    };
    download(ModelAsset::ParakeetV3);
    harness.wait_for("the speech model", |harness| {
        harness
            .sink
            .last(BridgeTopic::Onboarding)
            .is_some_and(|onboarding| {
                onboarding["speechModels"]["state"] == "absent"
                    && onboarding["speechModels"]["assetIDs"] == json!(["offlineDiarizer"])
            })
    });
    download(ModelAsset::OfflineDiarizer);
    harness.wait_for(
        "the failed download",
        onboarding_row_is("speechModels", "failed"),
    );
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["speechModels"],
        json!({
            "assetIDs": ["offlineDiarizer"],
            "detail": "Not downloaded · 22 MB",
            "failure": "offline",
            "state": "failed",
        })
    );
}

/// The row then reads installed or offers Retry with the reason; page 1 does
/// not move on by itself while it waits.
#[test]
fn the_speech_models_row_downloads_through_settings_and_follows_it() {
    let harness = handoff_without_the_diarizer()
        .seed(|_, fakes| {
            fakes.speech_models.fail_downloads(Some("offline"));
            fakes
                .permissions
                .set_state(PermissionKind::Calendar, PermissionState::Unknown);
        })
        .build();
    let ids = |harness: &Harness| -> Vec<String> {
        harness.snapshot(BridgeTopic::Onboarding)["speechModels"]["assetIDs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap().to_owned())
            .collect()
    };
    let download = |harness: &Harness| {
        for asset_id in ids(harness) {
            harness
                .host
                .for_window(BridgeWindow::Onboarding)
                .settings_transcription_download(steno_bridge::AssetIdParams { asset_id })
                .unwrap();
        }
    };
    download(&harness);
    harness.wait_for(
        "the failed download",
        onboarding_row_is("speechModels", "failed"),
    );
    let row = harness.snapshot(BridgeTopic::Onboarding)["speechModels"].clone();
    assert_eq!(row["failure"], "offline");
    assert_eq!(row["assetIDs"], json!(["offlineDiarizer"]));
    assert!(
        harness.sink.all(BridgeTopic::Onboarding).iter().any(
            |onboarding| onboarding["speechModels"]["state"] == "downloading"
                && onboarding["speechModels"]["downloadFraction"] == 0.5
                && onboarding["speechModels"]["detail"] == "Downloading… 50%"
        ),
        "the row showed the download's progress"
    );
    // While a model downloads, the row offers nothing to start.
    assert!(
        harness
            .sink
            .all(BridgeTopic::Onboarding)
            .iter()
            .filter(|onboarding| onboarding["speechModels"]["state"] == "downloading")
            .all(|onboarding| onboarding["speechModels"]["assetIDs"] == json!([]))
    );

    // Skipping the last permission does not move on while the row waits.
    harness
        .host
        .onboarding_skip(PermissionKindParams {
            kind: PermissionKind::Calendar,
        })
        .unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["page"],
        "permissions"
    );

    harness.fakes.speech_models.fail_downloads(None);
    download(&harness);
    harness.wait_for(
        "the install",
        onboarding_row_is("speechModels", "installed"),
    );
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["speechModels"],
        json!({ "assetIDs": [], "detail": "Installed", "state": "installed" })
    );
    assert_eq!(
        *harness.fakes.speech_models.downloads.lock().unwrap(),
        vec![ModelAsset::OfflineDiarizer, ModelAsset::OfflineDiarizer]
    );
    assert_eq!(
        harness.snapshot(BridgeTopic::SettingsTranscription)["allInstalled"],
        true
    );
}

/// The stored vault under Documents.
fn with_a_vault(store: &steno_core::Store, _: &steno_host::fakes::FakeServices) {
    store_vault(store, "/Users/nicolai/Documents/Work Vault");
}

fn missing_microphone(_: &steno_core::Store, fakes: &steno_host::fakes::FakeServices) {
    fakes
        .permissions
        .set_state(PermissionKind::Microphone, PermissionState::Unknown);
}

fn vault_checks(harness: &Harness) -> usize {
    harness
        .fakes
        .export_validator
        .validated
        .lock()
        .unwrap()
        .len()
}

fn vault_row_is(state: &'static str) -> impl Fn(&Harness) -> bool {
    onboarding_row_is("vaultAccess", state)
}

/// The row reads checking, then writable; nothing else checks again.
#[test]
fn the_stored_vault_is_checked_once_on_page_one() {
    let harness = builder()
        .seed(with_a_vault)
        .seed(missing_microphone)
        .build();
    harness.host.page_ready().unwrap();
    assert_eq!(vault_checks(&harness), 0, "not from the main window");
    assert!(harness.snapshot(BridgeTopic::Onboarding)["vaultAccess"].is_null());

    let onboarding = harness.host.for_window(BridgeWindow::Onboarding);
    onboarding.page_ready().unwrap();
    // The row reads checking before the write starts.
    let checking = json!({ "name": "Work Vault", "state": "checking" });
    assert!(
        harness
            .sink
            .all(BridgeTopic::Onboarding)
            .iter()
            .any(|snapshot| snapshot["vaultAccess"] == checking)
    );
    harness.wait_for("the check", vault_row_is("writable"));
    onboarding.page_ready().unwrap();
    onboarding.onboarding_refresh().unwrap();
    onboarding
        .onboarding_skip(PermissionKindParams {
            kind: PermissionKind::Calendar,
        })
        .unwrap();
    assert_eq!(vault_checks(&harness), 1);
    assert_eq!(
        harness.fakes.export_validator.validated.lock().unwrap()[0].vault_path,
        "/Users/nicolai/Documents/Work Vault"
    );
}

const NOT_WRITABLE: &str =
    "The Obsidian vault at /Users/nicolai/Documents/Work Vault is not writable.";

/// A refused write reads `refused` (a Files and Folders refusal: Open System
/// Settings opens that pane), a missing vault `unusable`; Check again checks
/// it again.
#[test]
fn a_refused_or_missing_vault_says_why_and_check_again_checks_it_again() {
    const MISSING: &str = "The Obsidian vault at /Volumes/Disk/Work Vault does not exist.";
    for (refused, state, message) in [
        (true, "refused", NOT_WRITABLE),
        (false, "unusable", MISSING),
    ] {
        let harness = builder()
            .seed(with_a_vault)
            .seed(missing_microphone)
            .seed(move |_, fakes| {
                if refused {
                    fakes.export_validator.refuse_writes(NOT_WRITABLE);
                } else {
                    fakes.export_validator.fail_validation(Some(MISSING));
                }
            })
            .build();
        let onboarding = harness.host.for_window(BridgeWindow::Onboarding);
        onboarding.page_ready().unwrap();
        harness.wait_for("the refusal", vault_row_is(state));
        assert_eq!(
            harness.snapshot(BridgeTopic::Onboarding)["vaultAccess"],
            json!({ "name": "Work Vault", "state": state, "validationMessage": message })
        );
        if refused {
            onboarding.onboarding_open_files_and_folders().unwrap();
            assert_eq!(
                *harness.fakes.opener.opened.lock().unwrap(),
                vec![steno_host::host::FILES_AND_FOLDERS_URL.to_owned()]
            );
        }

        harness.fakes.export_validator.fail_validation(None);
        onboarding.onboarding_refresh().unwrap();
        harness.wait_for("the second check", vault_row_is("writable"));
        assert_eq!(vault_checks(&harness), 2);
    }
}

/// The check waits for the import step.
#[test]
fn no_vault_no_mac_or_not_on_page_one_checks_nothing() {
    for vault in [None, Some("  ")] {
        let without = builder()
            .seed(move |store, _| {
                if let Some(path) = vault {
                    store_vault(store, path);
                }
            })
            .seed(missing_microphone)
            .build();
        without
            .host
            .for_window(BridgeWindow::Onboarding)
            .page_ready()
            .unwrap();
        assert_eq!(vault_checks(&without), 0, "{vault:?}");
        assert!(without.snapshot(BridgeTopic::Onboarding)["vaultAccess"].is_null());
    }

    // Denied counts as missing on Linux too, so the page stays on page 1.
    let linux = builder()
        .platform(Platform::Linux)
        .seed(with_a_vault)
        .seed(|_, fakes| {
            fakes
                .permissions
                .set_state(PermissionKind::Microphone, PermissionState::Denied);
        })
        .build();
    linux
        .host
        .for_window(BridgeWindow::Onboarding)
        .page_ready()
        .unwrap();
    assert_eq!(
        linux.snapshot(BridgeTopic::Onboarding)["page"],
        "permissions"
    );
    linux
        .host
        .for_window(BridgeWindow::Onboarding)
        .onboarding_open_files_and_folders()
        .unwrap();
    // No row either: a check would mark it checking before its thread runs.
    assert!(linux.snapshot(BridgeTopic::Onboarding)["vaultAccess"].is_null());
    assert_eq!(vault_checks(&linux), 0);
    assert!(linux.fakes.opener.opened.lock().unwrap().is_empty());

    let page_two = builder().seed(with_a_vault).build();
    page_two
        .host
        .for_window(BridgeWindow::Onboarding)
        .page_ready()
        .unwrap();
    assert_eq!(page_two.snapshot(BridgeTopic::Onboarding)["page"], "setup");
    assert!(page_two.snapshot(BridgeTopic::Onboarding)["vaultAccess"].is_null());
    assert_eq!(vault_checks(&page_two), 0);

    let import = builder()
        .with_swift_import(2)
        .seed(with_a_vault)
        .seed(missing_microphone)
        .build();
    let onboarding = import.host.for_window(BridgeWindow::Onboarding);
    onboarding.page_ready().unwrap();
    assert!(import.snapshot(BridgeTopic::Onboarding)["vaultAccess"].is_null());
    assert_eq!(vault_checks(&import), 0, "the import step comes first");
    onboarding.onboarding_import().unwrap();
    import.wait_for("the check after the step", vault_row_is("writable"));
    assert_eq!(vault_checks(&import), 1);
}

/// Every grant in place, the models downloaded after the vault was refused.
#[test]
fn check_again_with_everything_else_done_writes_again_and_stays_on_page_one() {
    let harness = handoff_without_the_diarizer()
        .seed(with_a_vault)
        .seed(|_, fakes| fakes.export_validator.refuse_writes(NOT_WRITABLE))
        .build();
    let onboarding = harness.host.for_window(BridgeWindow::Onboarding);
    onboarding.page_ready().unwrap();
    harness.wait_for("the refusal", vault_row_is("refused"));
    onboarding
        .settings_transcription_download(steno_bridge::AssetIdParams {
            asset_id: ModelAsset::OfflineDiarizer.as_str().to_owned(),
        })
        .unwrap();
    harness.wait_for(
        "the install",
        onboarding_row_is("speechModels", "installed"),
    );

    harness.fakes.export_validator.fail_validation(None);
    onboarding.onboarding_refresh().unwrap();
    harness.wait_for("the second check", vault_row_is("writable"));
    assert_eq!(vault_checks(&harness), 2);
    let snapshot = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(snapshot["page"], "permissions");
    assert_eq!(snapshot["finished"], false);
}

#[test]
fn a_refused_vault_holds_page_one_until_continue() {
    let harness = builder()
        .seed(with_a_vault)
        .seed(missing_microphone)
        .seed(|_, fakes| fakes.export_validator.refuse_writes(NOT_WRITABLE))
        .build();
    let onboarding = harness.host.for_window(BridgeWindow::Onboarding);
    onboarding.page_ready().unwrap();
    harness.wait_for("the refusal", vault_row_is("refused"));
    onboarding
        .onboarding_request(PermissionKindParams {
            kind: PermissionKind::Microphone,
        })
        .unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["permissionsComplete"],
        true
    );
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["page"],
        "permissions"
    );
    onboarding.onboarding_advance().unwrap();
    assert_eq!(harness.snapshot(BridgeTopic::Onboarding)["page"], "setup");
}
