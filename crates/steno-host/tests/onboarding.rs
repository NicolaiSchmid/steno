//! Onboarding through the host and the view model, ported from
//! `OnboardingViewModelTests` and `OnboardingBridgeTests`.

// The ported suites keep one Swift test per function, long as some are.
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

/// A harness whose speech models are installed, so onboarding shows no
/// speech models row: every test here but the ones about that row.
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

/// Swift: `testRetentionSentenceFollowsTheStoredRule`.
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

/// Swift: `testStepsRunInOrderAndRequiredOnesGateCompletion`, `testOptionalStepsCanBeSkippedRequiredCannot`,
/// `testRequestUpdatesOnlyTheRequestedStep`, `pageReadyPublishesAndPermissionCommandsRepublish`.
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

/// Swift: `testAlreadyGrantedPermissionsOpenOnTheSetupPage`, `testSkippingBothRowsFinishesAndSetsTheFlag`,
/// `testFinishWithRowsOpenSetsTheFlag`, `cancelledChooserSkipAndFinish`.
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
    // Finishing closes the window from the host, after the page saw
    // `finished` (Swift: `OnboardingWindow.onChange(of: finished)`).
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
    // The window closed; one opened again starts over (on the setup page,
    // every permission being granted), as Swift built a model per window,
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
    // Swift: `OnboardingWindow.onDisappear`.
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

/// The Summaries form sends the Settings window's method names from both
/// windows; each answers on its own model. Swift: `SummariesCommands` on
/// `OnboardingBridge.model.llm`.
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
    // A draft typed in the Settings window must not be what onboarding
    // commits.
    harness
        .host
        .settings_summaries_update(update(Some("settings-draft"), None))
        .unwrap();

    // selectPreset: the radio moves on page 2 and only there; what is
    // stored is onboarding's own draft (the address, no model yet).
    harness.sink.clear();
    onboarding
        .settings_summaries_select_preset(SetStringParams {
            value: "openAI".to_owned(),
        })
        .unwrap();
    let page = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(page["summaries"]["presetID"], "openAI");
    assert_eq!(page["summaries"]["baseURL"], "https://api.openai.com/v1");
    // The Settings section follows the store once another window wrote
    // it, as a reopened Swift window would: its unsaved draft is gone.
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
    // The Settings section follows the store once onboarding wrote it, as
    // a reopened Swift window would have.
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

/// Swift: `testSavingSummariesConfiguresTheEndpointAndRebuildsThePipeline`, `setupCommandsReachTheModelsAndFinish`.
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

/// Swift: `testSavingTheVaultValidatesThroughTheDestination`, `vaultRowReadsTheObsidianModel`.
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

/// Swift: `testChatGPTChoiceOnTheSummariesRow`, `testAStoredCodexConfigurationCollapsesTheSummariesRow`.
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

    // The onboarding form's own preset switch is the Settings method on the
    // host's Summaries model; the onboarding model follows the store on its
    // next load, as a stored Codex configuration collapses the row.
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

/// Swift: `testShouldOpenFollowsTheFlagAndTheRequiredPermissions`, `testConfiguredInstallWithoutTheFlagDoesNotOpen`.
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

/// Swift: `testCurrentIsDerivedForEveryPermissionCombination` and `testDeniedStepsStayCurrentAndOpenSettings`,
/// on the model alone.
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

/// The Swift import's step (Rust only): it comes before page 1 while the
/// import has work left, whatever the copied flag says, shows the prompts
/// while they are up, and moves on once the identity came over.
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

/// After the handoff (stable plan S3): the import copied the Swift app's
/// completed flag, but macOS keeps the grants per bundle identifier, so
/// the new app has none. Onboarding opens, and once the import step is
/// done it lands on its permissions page, not on page 2.
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

/// A second Continue or Try again while the first run's prompts are up
/// runs nothing: no second prompt comes up, and the page keeps showing
/// the prompts until the first run's outcome.
#[test]
fn a_second_import_while_one_runs_does_nothing() {
    let harness = builder().with_swift_import(2).build();
    let import = harness.fakes.swift_import.clone().unwrap();
    let host = harness.host.clone();
    let state = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = state.clone();
    *import.during_run.lock().unwrap() = Some(Box::new(move || {
        host.onboarding_import().unwrap();
        *seen.lock().unwrap() =
            Some(host.snapshot(BridgeTopic::Onboarding).unwrap()["swiftImport"]["state"].clone());
    }));
    harness.host.onboarding_import().unwrap();
    assert_eq!(*import.runs.lock().unwrap(), 1, "a second run");
    assert_eq!(
        state.lock().unwrap().take(),
        Some(json!("importing")),
        "the second click ended the prompts' state"
    );
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["swiftImport"]["state"],
        "done"
    );
}

/// Not now while Continue's prompts are up does nothing: the run decides,
/// and a run that leaves the identity behind stays on the step with Try
/// again.
#[test]
fn not_now_while_the_import_runs_leaves_the_step_to_the_run() {
    let harness = builder().with_swift_import(2).build();
    let import = harness.fakes.swift_import.clone().unwrap();
    import.deny_export("Steno could not bring over this Mac's phone pairing.");
    let host = harness.host.clone();
    let page = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = page.clone();
    *import.during_run.lock().unwrap() = Some(Box::new(move || {
        host.onboarding_skip_import().unwrap();
        let onboarding = host.snapshot(BridgeTopic::Onboarding).unwrap();
        *seen.lock().unwrap() = Some((
            onboarding["page"].clone(),
            onboarding["swiftImport"]["state"].clone(),
        ));
    }));
    harness.host.onboarding_import().unwrap();
    assert_eq!(*import.skips.lock().unwrap(), 0, "Not now skipped a run");
    assert_eq!(
        page.lock().unwrap().take(),
        Some((json!("import"), json!("importing")))
    );
    let onboarding = harness.snapshot(BridgeTopic::Onboarding);
    assert_eq!(onboarding["page"], "import");
    assert_eq!(onboarding["swiftImport"]["state"], "waiting");
}

/// The system audio test plays a tone a recording would pick up. While a
/// recording starts, runs or stops, neither onboarding nor Settings >
/// Recording runs it: no request reaches the services, the row never
/// shows Asking, and it says the test was refused for the recording.
/// Once the recorder is idle the line goes, and the next press runs the
/// test. A start during the test stops it (the services answer the state
/// as it was), and the row says so too.
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

    // A start while the test runs: the services stop it and answer the
    // state as it was.
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

/// The Swift app's handoff as the new app finds it: the `CoreML` speech
/// model in the Swift app's tree, so installed, but none of the speaker
/// models the Rust diarizer runs; the import copied the completed flag and
/// every grant is in place.
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

/// With only the speaker models missing, onboarding still opens at the
/// first launch after the Swift app, on page 1 with the speech models row
/// (not on page 2, though every permission is granted), and once this
/// app's onboarding was closed, missing models open it no more: Settings
/// and the meetings' own line say what is missing from then on.
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

/// A fresh install misses every model: the row offers both, sized
/// together.
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

/// On a fresh install where the speech model came in and the speaker
/// models failed, Retry starts only the speaker models, and the size is
/// theirs alone.
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

/// The row's Download is Settings' install (`settings.transcription.download`
/// per id the row lists): the row follows its progress, reads installed
/// afterwards and stays; a failed download offers Retry with the reason.
/// Page 1 does not move on by itself while the row waits for a model.
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

    // Skipping the last open permission does not move on while the row
    // waits; Continue still does.
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

/// The stored vault under Documents, as the Swift app left it.
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

/// Page 1 writes to the stored vault once, when the onboarding window's
/// page is ready, so macOS asks for a protected folder there: the row
/// reads checking, then writable. A second page ready and the page's
/// commands check nothing more; the main window's page ready never checks.
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
    assert_eq!(
        harness.sink.last(BridgeTopic::Onboarding).unwrap()["vaultAccess"],
        json!({ "name": "Work Vault", "state": "checking" })
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

/// The destination's refusal of a vault in Documents with Files and
/// Folders off.
const NOT_WRITABLE: &str =
    "The Obsidian vault at /Users/nicolai/Documents/Work Vault is not writable.";

/// A refused write shows the destination's reason; Check again writes
/// again, and Open System Settings opens Files and Folders.
#[test]
fn a_refused_vault_says_why_and_check_again_checks_it_again() {
    let harness = builder()
        .seed(with_a_vault)
        .seed(missing_microphone)
        .seed(|_, fakes| {
            fakes.export_validator.refuse_writes(NOT_WRITABLE);
        })
        .build();
    let onboarding = harness.host.for_window(BridgeWindow::Onboarding);
    onboarding.page_ready().unwrap();
    harness.wait_for("the refusal", vault_row_is("refused"));
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["vaultAccess"],
        json!({
            "name": "Work Vault",
            "state": "refused",
            "validationMessage": NOT_WRITABLE,
        })
    );
    onboarding.onboarding_open_files_and_folders().unwrap();
    assert_eq!(
        *harness.fakes.opener.opened.lock().unwrap(),
        vec![steno_host::host::FILES_AND_FOLDERS_URL.to_owned()]
    );

    harness.fakes.export_validator.fail_validation(None);
    onboarding.onboarding_refresh().unwrap();
    harness.wait_for("the second check", vault_row_is("writable"));
    assert_eq!(vault_checks(&harness), 2);
}

/// No vault stored, another platform, or a page other than page 1: no
/// write, no row. The import step comes before page 1, so the check waits
/// for the step to end.
#[test]
fn no_vault_no_mac_or_not_on_page_one_checks_nothing() {
    let without = builder().seed(missing_microphone).build();
    without
        .host
        .for_window(BridgeWindow::Onboarding)
        .page_ready()
        .unwrap();
    assert_eq!(vault_checks(&without), 0);
    assert!(without.snapshot(BridgeTopic::Onboarding)["vaultAccess"].is_null());

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

/// Nicolai's case (review M1 of the first launch): every grant in place,
/// the models downloaded from the row after the vault was refused. Page 1
/// does not move on by itself while the row is refused, and Check again
/// writes to the vault again instead of leaving page 1 (or closing
/// onboarding, with page 2 handled) without a new write.
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

/// With the models in, the last permission granted while the vault row is
/// refused leaves page 1 where it is; Continue still moves on.
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

/// A vault that is not there (an unmounted disk) is not a Files and
/// Folders refusal: the row reads unusable with the destination's words,
/// and Check again checks again.
#[test]
fn a_missing_vault_reads_unusable_and_check_again_checks_it_again() {
    let missing = "The Obsidian vault at /Volumes/Disk/Work Vault does not exist.";
    let harness = builder()
        .seed(with_a_vault)
        .seed(missing_microphone)
        .seed(move |_, fakes| fakes.export_validator.fail_validation(Some(missing)))
        .build();
    let onboarding = harness.host.for_window(BridgeWindow::Onboarding);
    onboarding.page_ready().unwrap();
    harness.wait_for("the refusal", vault_row_is("unusable"));
    assert_eq!(
        harness.snapshot(BridgeTopic::Onboarding)["vaultAccess"]["validationMessage"],
        missing
    );
    harness.fakes.export_validator.fail_validation(None);
    onboarding.onboarding_refresh().unwrap();
    harness.wait_for("the second check", vault_row_is("writable"));
    assert_eq!(vault_checks(&harness), 2);
}

/// A stored vault whose path is blank is no vault: no write, no row.
#[test]
fn a_blank_vault_path_is_not_checked() {
    let harness = builder()
        .seed(|store, _| store_vault(store, "  "))
        .seed(missing_microphone)
        .build();
    harness
        .host
        .for_window(BridgeWindow::Onboarding)
        .page_ready()
        .unwrap();
    assert_eq!(vault_checks(&harness), 0);
    assert!(harness.snapshot(BridgeTopic::Onboarding)["vaultAccess"].is_null());
}
