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
    let harness = Harness::builder().build();
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
    let harness = Harness::builder()
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
    let harness = Harness::builder().choose(None).build();
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

    let open_rows = Harness::builder().build();
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
    let closed = Harness::builder().build();
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
    let harness = Harness::builder()
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
    let harness = Harness::builder()
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
    let harness = Harness::builder()
        .choose(Some("/Users/nicolai/Notes"))
        .build();
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
    let harness = Harness::builder()
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
    let harness = Harness::builder()
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
    let stored = Harness::builder()
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
    let harness = Harness::builder().build();
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

    let configured = Harness::builder()
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
    let plain = Harness::builder().build();
    assert!(
        plain
            .snapshot(BridgeTopic::Onboarding)
            .get("swiftImport")
            .is_none(),
        "no import, no step"
    );

    let harness = Harness::builder()
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
        json!({"state": "ready", "prompts": 2})
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

#[test]
fn a_denied_export_stays_on_the_step_with_try_again_until_the_user_goes_on() {
    let harness = Harness::builder().with_swift_import(2).build();
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
    let harness = Harness::builder().with_swift_import(1).build();
    harness.host.onboarding_window_closed();
    let import = harness.fakes.swift_import.as_ref().unwrap();
    assert_eq!(*import.skips.lock().unwrap(), 1);
    assert_eq!(*import.runs.lock().unwrap(), 0);
}
