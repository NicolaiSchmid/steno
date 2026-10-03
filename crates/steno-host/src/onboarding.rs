//! Onboarding in two pages, after `Onboarding/OnboardingViewModel.swift`
//! and `Web/OnboardingSnapshots.swift`. Page 1, permissions: microphone,
//! system audio (both required), then calendar and local network
//! (optional). Page 2, "Summaries and export": the LLM endpoint and the
//! Obsidian vault, both optional, written through the same view models the
//! Settings sections use. The model owns the exit: Finish, or both rows
//! handled on page 2, set the completed flag and `finished`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use steno_bridge::{
    OnboardingPage, OnboardingPermissionStep, OnboardingSetupStep, OnboardingSetupStepKind,
    OnboardingSetupStepState, OnboardingSnapshot, OnboardingVault, PermissionKind, PermissionState,
};
use steno_core::paths::path_from_file_url;
use steno_core::{AudioRetention, Settings, Store};

use crate::labels::retention_footnote;
use crate::services::{Services, permission_is_required};
use crate::settings::llm::{CodexStatus, LlmPreset, LlmSettingsViewModel, url_host};
use crate::settings::obsidian::ObsidianSettingsViewModel;
use crate::settings::snapshots as settings_snapshots;
use crate::setup::{llm_configured, vault_configured};

/// Swift: `OnboardingViewModel.Step`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub kind: PermissionKind,
    pub state: PermissionState,
}

impl Step {
    #[must_use]
    pub fn is_required(&self) -> bool {
        permission_is_required(self.kind)
    }
}

/// Swift: `OnboardingViewModel.SetupState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupState {
    Open,
    /// Saved, with the collapsed row's line.
    Saved(String),
    Skipped,
}

impl SetupState {
    #[must_use]
    pub fn is_handled(&self) -> bool {
        *self != SetupState::Open
    }
}

/// The two optional rows of page 2, in order.
pub const SETUP_STEPS: [OnboardingSetupStepKind; 2] = [
    OnboardingSetupStepKind::Summaries,
    OnboardingSetupStepKind::Vault,
];

#[derive(Debug)]
pub struct OnboardingViewModel {
    pub steps: Vec<Step>,
    pub requesting: Option<PermissionKind>,
    pub skipped: BTreeSet<PermissionKind>,
    pub page: OnboardingPage,
    pub setup_states: BTreeMap<OnboardingSetupStepKind, SetupState>,
    /// Set by `finish`: the flag is written and the window should close.
    pub finished: bool,
    /// What happens to recordings under the stored rule; `None` until
    /// `load` ran.
    pub retention_sentence: Option<String>,
    pub llm: LlmSettingsViewModel,
    pub obsidian: ObsidianSettingsViewModel,
    loaded: bool,
}

impl Default for OnboardingViewModel {
    fn default() -> Self {
        Self::new()
    }
}

impl OnboardingViewModel {
    /// The preferences flag: this install has finished the two pages.
    pub const COMPLETED_KEY: &'static str = "steno.onboardingCompleted";

    #[must_use]
    pub fn new() -> Self {
        OnboardingViewModel {
            steps: PermissionKind::ALL
                .iter()
                .map(|kind| Step {
                    kind: *kind,
                    state: PermissionState::Unknown,
                })
                .collect(),
            requesting: None,
            skipped: BTreeSet::new(),
            page: OnboardingPage::Permissions,
            setup_states: SETUP_STEPS
                .iter()
                .map(|step| (*step, SetupState::Open))
                .collect(),
            finished: false,
            retention_sentence: None,
            llm: LlmSettingsViewModel::new(),
            obsidian: ObsidianSettingsViewModel::default(),
            loaded: false,
        }
    }

    /// Whether the opener shows the window: a required permission is
    /// missing, or this install has not finished the two pages yet. An
    /// install with the flag unset whose endpoint and vault are already in
    /// the settings has nothing left to ask: the flag is written and the
    /// window stays closed. Swift: `OnboardingViewModel.shouldOpen`.
    pub fn should_open(store: &Store, services: &Services) -> bool {
        if PermissionKind::ALL
            .iter()
            .filter(|kind| permission_is_required(**kind))
            .any(|kind| services.permissions.state(*kind) != PermissionState::Granted)
        {
            return true;
        }
        if services.preferences.flag(Self::COMPLETED_KEY) {
            return false;
        }
        if let Ok(stored) = store.settings()
            && llm_configured(&stored)
            && vault_configured(&stored)
        {
            services.preferences.set_flag(Self::COMPLETED_KEY, true);
            return false;
        }
        true
    }

    /// Permissions, the retention sentence and the setup rows. The page 2
    /// rows load once: a later `load` ("Check again" on page 1) refreshes
    /// the permissions and keeps whatever was typed on page 2.
    pub fn load(&mut self, store: &Store, services: &Services, secret: Option<String>) {
        for step in &mut self.steps {
            step.state = services.permissions.state(step.kind);
        }
        if let Ok(stored) = store.settings() {
            self.retention_sentence = Some(Self::retention_sentence_for(&stored));
        }
        if self.loaded {
            return;
        }
        self.loaded = true;
        self.llm.load(store, services, secret);
        if self.llm.is_configured {
            self.setup_states.insert(
                OnboardingSetupStepKind::Summaries,
                SetupState::Saved(Self::saved_line_llm(&self.llm)),
            );
        }
        self.obsidian.load(store);
        if self.obsidian.enabled {
            self.setup_states.insert(
                OnboardingSetupStepKind::Vault,
                SetupState::Saved(Self::saved_line_vault(&self.obsidian)),
            );
        }
        // An install that has the permissions but never saw page 2 starts
        // there; a fresh install starts on page 1.
        if self.is_complete() {
            self.page = OnboardingPage::Setup;
            self.finish_if_setup_handled(services);
        }
    }

    /// One sentence on what the rule does to the files, then where to
    /// change it. Swift: `OnboardingViewModel.retentionSentence(for:)`.
    #[must_use]
    pub fn retention_sentence_for(settings: &Settings) -> String {
        let rule = match settings.default_retention {
            AudioRetention::KeepForever => {
                let folder = path_from_file_url(&settings.audio_folder)
                    .and_then(|path| {
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                    })
                    .unwrap_or_default();
                format!("Recordings are kept forever in {folder}.")
            }
            rule @ (AudioRetention::KeepDays(_) | AudioRetention::DeleteAfterProcessing) => {
                retention_footnote(rule)
            }
        };
        format!("{rule} Change this any time in Settings > Audio.")
    }

    // Page 1

    /// The first step that is neither granted nor skipped; the last one
    /// when every step is handled.
    #[must_use]
    pub fn current(&self) -> PermissionKind {
        self.steps
            .iter()
            .find(|step| {
                step.state != PermissionState::Granted && !self.skipped.contains(&step.kind)
            })
            .map_or(PermissionKind::LocalNetwork, |step| step.kind)
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.steps
            .iter()
            .filter(|step| step.is_required())
            .all(|step| step.state == PermissionState::Granted)
    }

    /// Every permission step handled: required ones granted, optional ones
    /// granted or skipped. Page 1 advances on its own when this turns true.
    #[must_use]
    pub fn permissions_handled(&self) -> bool {
        self.is_complete()
            && self
                .steps
                .iter()
                .filter(|step| !step.is_required())
                .all(|step| {
                    step.state == PermissionState::Granted || self.skipped.contains(&step.kind)
                })
    }

    /// Both setup rows saved or skipped.
    #[must_use]
    pub fn setup_handled(&self) -> bool {
        SETUP_STEPS
            .iter()
            .all(|step| self.setup_state(*step).is_handled())
    }

    #[must_use]
    pub fn state_of(&self, kind: PermissionKind) -> PermissionState {
        self.steps
            .iter()
            .find(|step| step.kind == kind)
            .map_or(PermissionState::Unknown, |step| step.state)
    }

    /// Marks the step as requesting; the host publishes, runs the system
    /// prompt and then calls [`Self::finish_request`]. Swift's `request`
    /// did the three in one `async` method.
    pub fn begin_request(&mut self, kind: PermissionKind) {
        self.requesting = Some(kind);
    }

    pub fn finish_request(&mut self, kind: PermissionKind, state: PermissionState) {
        if let Some(step) = self.steps.iter_mut().find(|step| step.kind == kind) {
            step.state = state;
        }
        self.requesting = None;
    }

    /// Optional steps can be skipped; required ones cannot.
    pub fn skip(&mut self, kind: PermissionKind) {
        if !permission_is_required(kind) {
            self.skipped.insert(kind);
        }
    }

    /// Done or Later on page 1. Page 2 with both rows already handled has
    /// nothing to show, so it finishes.
    pub fn advance(&mut self, services: &Services) {
        self.page = OnboardingPage::Setup;
        self.finish_if_setup_handled(services);
    }

    /// Back on page 2.
    pub fn back(&mut self) {
        self.page = OnboardingPage::Permissions;
    }

    // Page 2

    #[must_use]
    pub fn setup_state(&self, step: OnboardingSetupStepKind) -> SetupState {
        self.setup_states
            .get(&step)
            .cloned()
            .unwrap_or(SetupState::Open)
    }

    pub fn skip_setup(&mut self, step: OnboardingSetupStepKind, services: &Services) {
        self.setup_states.insert(step, SetupState::Skipped);
        self.finish_if_setup_handled(services);
    }

    /// The Summaries row's Save can go: a valid URL, a model name and
    /// nothing the view model rejects. The `ChatGPT` choice has no Save: its
    /// consent button is the save.
    #[must_use]
    pub fn can_save_summaries(&self) -> bool {
        self.llm.preset != LlmPreset::Codex
            && self.llm.validation_message().is_none()
            && self.llm.base_url().is_some()
            && !self.llm.model.trim().is_empty()
    }

    /// Saves through the LLM view model (same validation, same pipeline
    /// rebuild) and collapses the row on success.
    pub fn save_summaries(&mut self, store: &Store, services: &Services, now: DateTime<Utc>) {
        if !self.can_save_summaries() {
            return;
        }
        self.llm.save(store, services, now);
        self.collapse_summaries_if_saved(services);
    }

    /// The consent card's button on the Summaries row.
    pub fn confirm_summaries_with_codex(
        &mut self,
        store: &Store,
        services: &Services,
        now: DateTime<Utc>,
    ) {
        if self.llm.preset != LlmPreset::Codex {
            return;
        }
        self.llm.confirm_codex(store, services, now);
        self.collapse_summaries_if_saved(services);
    }

    /// The Summaries row collapses once the LLM view model saved a
    /// configured endpoint without error.
    fn collapse_summaries_if_saved(&mut self, services: &Services) {
        if self.llm.errors.error.is_some() || !self.llm.is_configured {
            return;
        }
        self.setup_states.insert(
            OnboardingSetupStepKind::Summaries,
            SetupState::Saved(Self::saved_line_llm(&self.llm)),
        );
        self.finish_if_setup_handled(services);
    }

    /// A folder from the chooser becomes the vault and is saved at once.
    pub fn choose_vault(&mut self, folder: &Path, store: &Store, services: &Services) {
        self.obsidian.vault_path = folder.to_string_lossy().into_owned();
        self.save_vault(store, services);
    }

    /// Saves through the Obsidian view model (validated by the destination)
    /// and collapses the row on success.
    pub fn save_vault(&mut self, store: &Store, services: &Services) {
        self.obsidian.enabled = true;
        self.obsidian.save(store, services);
        if !self.obsidian.saved || self.obsidian.errors.error.is_some() {
            return;
        }
        self.setup_states.insert(
            OnboardingSetupStepKind::Vault,
            SetupState::Saved(Self::saved_line_vault(&self.obsidian)),
        );
        self.finish_if_setup_handled(services);
    }

    // Exit

    /// The flag alone, for the window's close button.
    pub fn mark_completed(services: &Services) {
        services.preferences.set_flag(Self::COMPLETED_KEY, true);
    }

    /// Finish, or both rows handled on page 2: the flag, then `finished`.
    pub fn finish(&mut self, services: &Services) {
        Self::mark_completed(services);
        self.finished = true;
    }

    fn finish_if_setup_handled(&mut self, services: &Services) {
        if self.page == OnboardingPage::Setup && self.setup_handled() {
            self.finish(services);
        }
    }

    /// `Saved: <model> at <host>` or `Saved: <model> via ChatGPT as <account>`.
    #[must_use]
    pub fn saved_line_llm(llm: &LlmSettingsViewModel) -> String {
        if llm.preset == LlmPreset::Codex {
            let account = match &llm.codex_status {
                CodexStatus::SignedIn(line) => format!(" as {line}"),
                _ => String::new(),
            };
            return format!("Saved: {} via ChatGPT{account}", llm.codex_model);
        }
        let host = llm
            .base_url()
            .and_then(|url| url_host(&url))
            .unwrap_or_default();
        format!("Saved: {} at {host}", llm.model.trim())
    }

    /// `Saved: <vault folder name>`.
    #[must_use]
    pub fn saved_line_vault(obsidian: &ObsidianSettingsViewModel) -> String {
        let name = Path::new(&obsidian.vault_path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("Saved: {name}")
    }
}

/// The onboarding window's topic. Page 2 only carries the Summaries form
/// (the Settings snapshot without a sidebar subtitle) and the vault row.
#[must_use]
pub fn snapshot(model: &OnboardingViewModel) -> OnboardingSnapshot {
    let on_setup = model.page == OnboardingPage::Setup;
    OnboardingSnapshot {
        page: model.page,
        permissions: model
            .steps
            .iter()
            .map(|step| OnboardingPermissionStep {
                kind: step.kind,
                state: step.state,
                is_required: step.is_required(),
                is_requesting: model.requesting == Some(step.kind),
                is_skipped: model.skipped.contains(&step.kind),
            })
            .collect(),
        permissions_complete: model.is_complete(),
        setup: SETUP_STEPS
            .iter()
            .map(|step| {
                let (state, saved_line) = match model.setup_state(*step) {
                    SetupState::Open => (OnboardingSetupStepState::Open, None),
                    SetupState::Saved(line) => (OnboardingSetupStepState::Saved, Some(line)),
                    SetupState::Skipped => (OnboardingSetupStepState::Skipped, None),
                };
                OnboardingSetupStep {
                    kind: *step,
                    state,
                    saved_line,
                }
            })
            .collect(),
        can_save_summaries: model.can_save_summaries(),
        summaries: on_setup.then(|| settings_snapshots::summaries(&model.llm, "")),
        vault: on_setup.then(|| {
            let vault = model.obsidian.vault_url();
            OnboardingVault {
                path: vault
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
                name: vault.is_some().then(|| model.obsidian.vault_name()),
                validation_message: model.obsidian.validation_message.clone(),
                error: model.obsidian.errors.error.clone(),
                error_details: model.obsidian.errors.details.clone(),
            }
        }),
        retention_sentence: model.retention_sentence.clone(),
        finished: model.finished,
    }
}
