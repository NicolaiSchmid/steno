//! Onboarding in two pages, after `Onboarding/OnboardingViewModel.swift`
//! and `Web/OnboardingSnapshots.swift`, with an import step before them on
//! the Mac's first launch after the update from the Swift app (Rust only,
//! [`SwiftImport`](crate::services::SwiftImport)): it explains the
//! keychain prompts, runs the import and offers Try again while the
//! handover identity has not come over. Page 1, permissions: microphone,
//! system audio (both required), then calendar and local network
//! (optional), as far as the platform has them
//! (`PermissionKind::for_platform`: all four on the Mac, fewer on Windows
//! and Linux). Page 2, "Summaries and export": the LLM endpoint and the
//! Obsidian vault, both optional, written through the same view models the
//! Settings sections use. The model owns the exit: Finish, or both rows
//! handled on page 2, set the completed flag and `finished`.
//!
//! Page 1 has two more rows, Rust only, for the first launch after the
//! Swift app (`.plans/2026-10-07-stable-promotion.md`, "The Sparkle
//! handoff", "First launch after the handoff"): the speech models row
//! while the models a meeting needs were missing when onboarding opened
//! (the speaker models the Swift app never had, or every model on a fresh
//! install), whose Download is Settings' install, and on the Mac the vault
//! row, which writes the destination's probe to the stored vault (a hidden
//! file, removed at once) so that macOS asks for its folder while the page
//! shows rather than at the first export. While that row is checking or
//! refused, page 1 does not move on by itself.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use steno_bridge::{
    OnboardingImport, OnboardingImportState, OnboardingPage, OnboardingPermissionStep,
    OnboardingSetupStep, OnboardingSetupStepKind, OnboardingSetupStepState, OnboardingSnapshot,
    OnboardingSpeechModels, OnboardingVault, OnboardingVaultAccess, OnboardingVaultAccessState,
    PermissionKind, PermissionState, Platform, TranscriptionAssetState,
};
use steno_core::protocols::BoundaryResult;
use steno_core::{AudioRetention, ObsidianSettings, Settings, Store};

use crate::labels::{file_size, retention_footnote};
use crate::services::{
    CodexModel, CodexModelsError, Services, SpeechModels, SwiftImportStage, SwiftImportStatus,
    VaultRefusal, permission_is_missing, permission_is_required,
};
use crate::settings::llm::{CodexStatus, KeyRead, LlmPreset, LlmSettingsViewModel, url_host};
use crate::settings::obsidian::{ObsidianSettingsViewModel, VaultSave};
use crate::settings::snapshots as settings_snapshots;
use crate::settings::transcription::downloading_text;
use crate::settings::{AssetState, SpeechSettingsViewModel};
use crate::setup::{llm_configured, vault_configured};
use crate::speech::{ModelAsset, SpeechEngineId};

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

/// Page 1's vault row: the stored vault's folder name and whether it took
/// the probe write. Rust only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultCheck {
    pub name: String,
    pub access: VaultAccess,
}

/// Whether the stored vault took the probe write. Rust only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultAccess {
    /// Check again was chosen after a refusal: the next check is due, and
    /// the row reads checking until it has run.
    Due,
    /// The write runs; macOS may be asking for the folder.
    Checking,
    Writable,
    Refused(VaultRefusal),
}

/// The two optional rows of page 2, in order.
pub const SETUP_STEPS: [OnboardingSetupStepKind; 2] = [
    OnboardingSetupStepKind::Summaries,
    OnboardingSetupStepKind::Vault,
];

#[derive(Debug)]
pub struct OnboardingViewModel {
    /// Which permissions page 1 lists and how the Summaries presets read.
    pub platform: Platform,
    pub steps: Vec<Step>,
    pub requesting: Option<PermissionKind>,
    /// The permission whose last request was refused, or stopped, for the
    /// recording (the system audio test, which the host refuses while a
    /// recording starts, runs or stops); cleared when the recorder is idle
    /// again, or by the next request.
    pub waits_for_recording: Option<PermissionKind>,
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
    /// The import step's status, from the first `load` on, when the
    /// services had an import with work left; `None` without a step.
    pub import: Option<SwiftImportStatus>,
    /// The import's keychain prompts are up.
    pub importing: bool,
    /// The models a meeting needs that were missing at the first `load`:
    /// page 1's speech models row, which stays, reading installed, once
    /// they arrive. Empty without the row. Rust only.
    pub missing_models: Vec<ModelAsset>,
    /// Page 1's vault row, once its check started. Rust only.
    pub vault_check: Option<VaultCheck>,
    loaded: bool,
}

impl Default for OnboardingViewModel {
    fn default() -> Self {
        Self::new(Platform::CURRENT)
    }
}

impl OnboardingViewModel {
    /// The preferences flag: this install has finished the two pages.
    pub const COMPLETED_KEY: &'static str = "steno.onboardingCompleted";

    /// The preferences flag: this app's onboarding was closed or finished
    /// once, so it has offered the speech models. Unlike
    /// [`Self::COMPLETED_KEY`], the Swift import never copies it, so the
    /// first launch after the Swift app opens onboarding while the models
    /// are missing even when its grants were in place. Rust only.
    pub const MODELS_OFFERED_KEY: &'static str = "steno.speechModelsOffered";

    /// Page 1 lists the permissions `platform` has.
    #[must_use]
    pub fn new(platform: Platform) -> Self {
        OnboardingViewModel {
            platform,
            steps: PermissionKind::for_platform(platform)
                .iter()
                .map(|kind| Step {
                    kind: *kind,
                    state: PermissionState::Unknown,
                })
                .collect(),
            requesting: None,
            waits_for_recording: None,
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
            import: None,
            importing: false,
            missing_models: Vec::new(),
            vault_check: None,
            loaded: false,
        }
    }

    /// Whether the opener shows the window: a required permission is
    /// missing ([`permission_is_missing`]), or this install has not
    /// finished the two pages yet. A missing permission opens it whatever
    /// the completed flag says, so after the update from the Swift app,
    /// whose flag the import copies but whose grants macOS keeps for the
    /// Swift app alone, onboarding opens on its permissions page (stable
    /// plan S3). An install with the flag unset whose endpoint and vault
    /// are already in the settings has nothing left to ask: the flag is
    /// written and the window stays closed. Swift:
    /// `OnboardingViewModel.shouldOpen`, where every state but `granted`
    /// was missing. The import step opens it too, whatever the flag says,
    /// until the identity came over, and so do missing speech models until
    /// this app's onboarding offered them once ([`Self::MODELS_OFFERED_KEY`])
    /// (Rust only).
    pub fn should_open(store: &Store, services: &Services, platform: Platform) -> bool {
        if import_open(services).is_some() {
            return true;
        }
        if !services.preferences.flag(Self::MODELS_OFFERED_KEY)
            && !missing_models(store, services).is_empty()
        {
            return true;
        }
        if PermissionKind::for_platform(platform)
            .iter()
            .filter(|kind| permission_is_required(**kind))
            .any(|kind| permission_is_missing(platform, services.permissions.state(*kind)))
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
    /// the permissions and keeps whatever was typed on page 2, so it
    /// ignores `key`.
    pub fn load(&mut self, store: &Store, services: &Services, key: KeyRead) {
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
        self.llm.load(store, services, key);
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
        self.missing_models = missing_models(store, services);
        // The import step comes first while it has work left.
        self.import = import_open(services);
        if self.import.is_some() {
            self.page = OnboardingPage::Import;
            return;
        }
        // An install that has the permissions but never saw page 2 starts
        // there, unless the models row waits on page 1; a fresh install
        // starts on page 1.
        if self.is_complete() && !self.models_pending(services) {
            self.page = OnboardingPage::Setup;
            self.finish_if_setup_handled(services);
        }
    }

    /// One sentence on what the rule does to the files, then where to
    /// change it: Settings > Recording, the section that holds the rule.
    /// Swift: `OnboardingViewModel.retentionSentence(for:)`.
    #[must_use]
    pub fn retention_sentence_for(settings: &Settings) -> String {
        let rule = match settings.default_retention {
            AudioRetention::KeepForever => "Recordings are kept until you delete them.".to_owned(),
            rule @ (AudioRetention::KeepDays(_) | AudioRetention::DeleteAfterProcessing) => {
                retention_footnote(rule)
            }
        };
        format!("{rule} Change this any time in Settings > Recording.")
    }

    // The import step

    /// The keychain prompts are about to come up; the host publishes, runs
    /// [`SwiftImport::run`](crate::services::SwiftImport::run) with its
    /// lock released, then calls [`Self::finish_import`]. `false` while a
    /// run is already under way, which the host then leaves to itself.
    pub fn begin_import(&mut self) -> bool {
        !std::mem::replace(&mut self.importing, true)
    }

    /// The run's or the skip's outcome. A finished import, or a skip,
    /// moves on to page 1 (or page 2 when the permissions are already
    /// granted); a run that left the identity behind stays on the step
    /// with Try again. `key` is the API key as the secret store reads it
    /// now, which the import may have just brought over: the Summaries
    /// row loads again with it.
    pub fn finish_import(
        &mut self,
        status: SwiftImportStatus,
        skipped: bool,
        store: &Store,
        services: &Services,
        key: KeyRead,
    ) {
        self.importing = false;
        let leave = skipped || status.stage == SwiftImportStage::Done;
        self.import = Some(status);
        if !leave || self.page != OnboardingPage::Import {
            return;
        }
        if !self
            .setup_state(OnboardingSetupStepKind::Summaries)
            .is_handled()
        {
            self.llm.load(store, services, key);
            if self.llm.is_configured {
                self.setup_states.insert(
                    OnboardingSetupStepKind::Summaries,
                    SetupState::Saved(Self::saved_line_llm(&self.llm)),
                );
            }
        }
        self.page = OnboardingPage::Permissions;
        if self.is_complete() && !self.models_pending(services) {
            self.page = OnboardingPage::Setup;
            self.finish_if_setup_handled(services);
        }
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
            .or_else(|| self.steps.last())
            .map_or(PermissionKind::Microphone, |step| step.kind)
    }

    /// No required step is missing on this platform
    /// ([`permission_is_missing`]): page 1's Continue, and where a
    /// launch with the permissions in place starts.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.steps
            .iter()
            .filter(|step| step.is_required())
            .all(|step| !permission_is_missing(self.platform, step.state))
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

    /// The speech models row shows a model that is not installed yet, so
    /// page 1 neither starts on page 2 nor moves on by itself.
    #[must_use]
    pub fn models_pending(&self, services: &Services) -> bool {
        self.missing_models
            .iter()
            .any(|asset| !services.speech_models.is_installed(*asset))
    }

    /// The vault row's check is due: on the Mac, on page 1, with a vault
    /// stored, once per window and again after Check again. Marks the row
    /// checking and returns the stored settings the host writes to with its
    /// lock released ([`ExportValidator`](crate::services::ExportValidator)).
    /// Never for a vault that is not stored; off the Mac there is no folder
    /// prompt to bring forward, so nothing is checked.
    pub fn begin_vault_check(&mut self, store: &Store) -> Option<ObsidianSettings> {
        if self.platform != Platform::Macos
            || self.page != OnboardingPage::Permissions
            || self
                .vault_check
                .as_ref()
                .is_some_and(|check| check.access != VaultAccess::Due)
        {
            return None;
        }
        let Some(obsidian) = store
            .settings()
            .ok()
            .and_then(|settings| settings.obsidian)
            .filter(|obsidian| !obsidian.vault_path.trim().is_empty())
        else {
            self.vault_check = None;
            return None;
        };
        let name = Path::new(&obsidian.vault_path).file_name().map_or_else(
            || obsidian.vault_path.clone(),
            |name| name.to_string_lossy().into_owned(),
        );
        self.vault_check = Some(VaultCheck {
            name,
            access: VaultAccess::Checking,
        });
        Some(obsidian)
    }

    /// The check's outcome, on the row it started on (a window closed
    /// meanwhile starts a model without one, which this leaves alone).
    pub fn finish_vault_check(&mut self, outcome: Result<(), VaultRefusal>) {
        if let Some(check) = &mut self.vault_check
            && check.access == VaultAccess::Checking
        {
            check.access = match outcome {
                Ok(()) => VaultAccess::Writable,
                Err(refusal) => VaultAccess::Refused(refusal),
            };
        }
    }

    /// "Check again" on page 1 makes the check of a refused vault due
    /// again, after the user turned Steno on under Files and Folders or
    /// brought the folder back.
    pub fn retry_vault_check(&mut self) {
        if let Some(check) = &mut self.vault_check
            && matches!(check.access, VaultAccess::Refused(_))
        {
            check.access = VaultAccess::Due;
        }
    }

    /// The vault row holds page 1: its check is due, runs, or was refused,
    /// so the page does not move on by itself before the user saw it.
    #[must_use]
    pub fn vault_pending(&self) -> bool {
        self.vault_check
            .as_ref()
            .is_some_and(|check| check.access != VaultAccess::Writable)
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
        self.waits_for_recording = None;
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

    /// Continue or Later on page 1. Page 2 with both rows already handled has
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

    /// The consent card's button on the Summaries row; true when the
    /// model list is to be fetched next, which the host does with its lock
    /// released before [`Self::finish_confirm_summaries_with_codex`].
    pub fn begin_confirm_summaries_with_codex(
        &mut self,
        store: &Store,
        services: &Services,
        now: DateTime<Utc>,
    ) -> bool {
        if self.llm.preset != LlmPreset::Codex {
            return false;
        }
        if self.llm.begin_confirm_codex(store, services, now) {
            return true;
        }
        self.collapse_summaries_if_saved(services);
        false
    }

    /// The fetched list applied and saved, then the row collapses once it
    /// is configured.
    pub fn finish_confirm_summaries_with_codex(
        &mut self,
        account: BoundaryResult<String>,
        models: Result<Vec<CodexModel>, CodexModelsError>,
        store: &Store,
        services: &Services,
        now: DateTime<Utc>,
    ) {
        self.llm
            .finish_confirm_codex(account, models, store, services, now);
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
    pub fn choose_vault(&mut self, folder: &Path) -> VaultSave {
        self.obsidian.vault_path = folder.to_string_lossy().into_owned();
        self.begin_save_vault()
    }

    /// Begins a save through the Obsidian view model; the host validates
    /// it through the destination with its lock released, then
    /// [`finish_save_vault`](Self::finish_save_vault) stores it.
    pub fn begin_save_vault(&mut self) -> VaultSave {
        self.obsidian.enabled = true;
        self.obsidian.begin_save()
    }

    /// Stores the save [`begin_save_vault`](Self::begin_save_vault) began
    /// and collapses the row on success.
    pub fn finish_save_vault(
        &mut self,
        save: VaultSave,
        validated: Result<(), VaultRefusal>,
        store: &Store,
        services: &Services,
    ) {
        self.obsidian.finish_save(save, validated, store);
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

    /// The flags alone, for the window's close button: the pages were
    /// seen, the speech models offered.
    pub fn mark_completed(services: &Services) {
        services.preferences.set_flag(Self::COMPLETED_KEY, true);
        services
            .preferences
            .set_flag(Self::MODELS_OFFERED_KEY, true);
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

/// The models a meeting needs under the stored engine, the ones Settings'
/// Transcription section lists, that are not installed.
fn missing_models(store: &Store, services: &Services) -> Vec<ModelAsset> {
    let engine = store
        .settings()
        .ok()
        .and_then(|settings| settings.speech_engine_id.parse().ok())
        .unwrap_or(SpeechEngineId::ParakeetV3);
    [engine.asset(), ModelAsset::OfflineDiarizer]
        .into_iter()
        .filter(|asset| !services.speech_models.is_installed(*asset))
        .collect()
}

/// The speech models row: one line for the models that were missing,
/// over the download states Settings' Transcription section keeps, so a
/// download started on either page shows on both.
fn speech_models_snapshot(
    model: &OnboardingViewModel,
    speech: &SpeechSettingsViewModel,
    models: &dyn SpeechModels,
) -> Option<OnboardingSpeechModels> {
    if model.missing_models.is_empty() {
        return None;
    }
    let rows: Vec<(ModelAsset, AssetState, i64)> = model
        .missing_models
        .iter()
        .map(|asset| {
            (
                *asset,
                speech.state_of(*asset),
                models.expected_bytes(*asset),
            )
        })
        .collect();
    let installed = |state: &AssetState| matches!(state, AssetState::Installed { .. });
    if rows.iter().all(|(_, state, _)| installed(state)) {
        return Some(OnboardingSpeechModels {
            state: TranscriptionAssetState::Installed,
            detail: "Installed".to_owned(),
            asset_ids: Vec::new(),
            download_fraction: None,
            failure: None,
        });
    }
    if rows
        .iter()
        .any(|(_, state, _)| matches!(state, AssetState::Downloading { .. }))
    {
        // The share of the bytes in place, an installed model counting whole.
        let total: i64 = rows.iter().map(|(_, _, bytes)| bytes).sum();
        #[allow(
            clippy::cast_precision_loss,
            reason = "a share of a few hundred megabytes"
        )]
        let fraction = (total > 0).then(|| {
            rows.iter()
                .map(|(_, state, bytes)| {
                    let done = match state {
                        AssetState::Installed { .. } => 1.0,
                        AssetState::Downloading { fraction, .. } => *fraction,
                        AssetState::Absent | AssetState::Failed(_) => 0.0,
                    };
                    done * *bytes as f64
                })
                .sum::<f64>()
                / total as f64
        });
        return Some(OnboardingSpeechModels {
            state: TranscriptionAssetState::Downloading,
            detail: downloading_text(fraction.unwrap_or(0.0)),
            asset_ids: Vec::new(),
            download_fraction: fraction,
            failure: None,
        });
    }
    // Neither installed nor downloading: Download or Retry starts the rest.
    let failure = rows.iter().find_map(|(_, state, _)| match state {
        AssetState::Failed(failure) => Some(failure.clone()),
        _ => None,
    });
    let missing: Vec<_> = rows
        .iter()
        .filter(|(_, state, _)| !installed(state))
        .collect();
    Some(OnboardingSpeechModels {
        state: if failure.is_some() {
            TranscriptionAssetState::Failed
        } else {
            TranscriptionAssetState::Absent
        },
        detail: format!(
            "Not downloaded · {}",
            file_size(missing.iter().map(|(_, _, bytes)| bytes).sum())
        ),
        asset_ids: missing
            .iter()
            .map(|(asset, _, _)| asset.as_str().to_owned())
            .collect(),
        download_fraction: None,
        failure,
    })
}

/// The vault row as the page shows it.
fn vault_access_snapshot(model: &OnboardingViewModel) -> Option<OnboardingVaultAccess> {
    let check = model.vault_check.as_ref()?;
    let (state, validation_message) = match &check.access {
        VaultAccess::Due | VaultAccess::Checking => (OnboardingVaultAccessState::Checking, None),
        VaultAccess::Writable => (OnboardingVaultAccessState::Writable, None),
        VaultAccess::Refused(refusal) => (
            if refusal.not_writable {
                OnboardingVaultAccessState::Refused
            } else {
                OnboardingVaultAccessState::Unusable
            },
            Some(refusal.message.clone()),
        ),
    };
    Some(OnboardingVaultAccess {
        state,
        name: check.name.clone(),
        validation_message,
    })
}

/// The import's status while it has work left (pending or waiting).
fn import_open(services: &Services) -> Option<SwiftImportStatus> {
    services
        .swift_import
        .as_ref()
        .map(|import| import.status())
        .filter(|status| status.stage != SwiftImportStage::Done)
}

/// The import step as the page shows it.
fn import_snapshot(model: &OnboardingViewModel) -> Option<OnboardingImport> {
    let status = model.import.as_ref()?;
    let state = if model.importing {
        OnboardingImportState::Importing
    } else {
        match status.stage {
            SwiftImportStage::Pending => OnboardingImportState::Pending,
            SwiftImportStage::Waiting => OnboardingImportState::Waiting,
            SwiftImportStage::Done => OnboardingImportState::Done,
        }
    };
    Some(OnboardingImport {
        state,
        prompts: i64::from(status.prompts),
        error: status.error.clone(),
    })
}

/// The onboarding window's topic. Page 2 only carries the Summaries form
/// (the Settings snapshot without a sidebar subtitle) and the vault row;
/// page 1's speech models row reads the download states of `speech`,
/// Settings' Transcription section, and the sizes `models` expects.
#[must_use]
pub fn snapshot(
    model: &OnboardingViewModel,
    speech: &SpeechSettingsViewModel,
    models: &dyn SpeechModels,
) -> OnboardingSnapshot {
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
                waits_for_recording: model.waits_for_recording == Some(step.kind),
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
        summaries: on_setup.then(|| settings_snapshots::summaries(&model.llm, "", model.platform)),
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
        swift_import: import_snapshot(model),
        speech_models: speech_models_snapshot(model, speech, models),
        vault_access: vault_access_snapshot(model),
        finished: model.finished,
    }
}
