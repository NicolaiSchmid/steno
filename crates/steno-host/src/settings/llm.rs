//! Summaries: the preset, the OpenAI-compatible base URL, model, context
//! tokens, and the API key in the secret store (never in `Settings`); or,
//! for the `ChatGPT` preset, the confirmation that Steno may use the Codex
//! sign-in and the model picked from the backend's list.
//! Swift: `Settings/LLMSettingsViewModel.swift`.

use chrono::{DateTime, Utc};
use steno_core::protocols::SecretKey;
use steno_core::{LlmProvider, Settings, Store, string_enum};

use super::{SectionError, update_settings};
use crate::services::{CodexModel, CodexModelsError, Services};
use crate::setup::llm_configured;

string_enum! {
    /// The services the Summaries section offers by name. Swift: `LLMPreset`.
    pub enum LlmPreset {
        LmStudio = "lmStudio",
        Ollama = "ollama",
        Codex = "codex",
        OpenRouter = "openRouter",
        OpenAi = "openAI",
        Anthropic = "anthropic",
        Custom = "custom",
    }
}

impl LlmPreset {
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            LlmPreset::LmStudio => "LM Studio on this Mac",
            LlmPreset::Ollama => "Ollama on this Mac",
            LlmPreset::Codex => "ChatGPT (Codex)",
            LlmPreset::OpenRouter => "OpenRouter",
            LlmPreset::OpenAi => "OpenAI",
            LlmPreset::Anthropic => "Anthropic",
            LlmPreset::Custom => "Custom server",
        }
    }

    /// `None` for Custom (the user types the address) and for `ChatGPT`.
    #[must_use]
    pub fn base_url(self) -> Option<&'static str> {
        match self {
            LlmPreset::LmStudio => Some("http://127.0.0.1:1234/v1"),
            LlmPreset::Ollama => Some("http://127.0.0.1:11434/v1"),
            LlmPreset::OpenRouter => Some("https://openrouter.ai/api/v1"),
            LlmPreset::OpenAi => Some("https://api.openai.com/v1"),
            LlmPreset::Anthropic => Some("https://api.anthropic.com/v1"),
            LlmPreset::Codex | LlmPreset::Custom => None,
        }
    }

    #[must_use]
    pub fn provider(self) -> LlmProvider {
        if self == LlmPreset::Codex {
            LlmProvider::Codex
        } else {
            LlmProvider::Endpoint
        }
    }

    #[must_use]
    pub fn needs_api_key(self) -> bool {
        matches!(
            self,
            LlmPreset::OpenRouter | LlmPreset::OpenAi | LlmPreset::Anthropic
        )
    }

    #[must_use]
    pub fn shows_server_field(self) -> bool {
        matches!(
            self,
            LlmPreset::LmStudio | LlmPreset::Ollama | LlmPreset::Custom
        )
    }

    #[must_use]
    pub fn model_placeholder(self) -> &'static str {
        match self {
            LlmPreset::LmStudio => "the model loaded in LM Studio",
            LlmPreset::Ollama => "llama3.1",
            LlmPreset::Codex => "pick a model",
            LlmPreset::OpenRouter => "openai/gpt-4.1-mini",
            LlmPreset::OpenAi => "gpt-4.1-mini",
            LlmPreset::Anthropic => "claude-sonnet-5",
            LlmPreset::Custom => "model name",
        }
    }

    /// `ChatGPT` when that provider is stored, else the preset whose address
    /// matches.
    #[must_use]
    pub fn infer_from_settings(settings: &Settings) -> LlmPreset {
        if settings.llm_provider == LlmProvider::Codex {
            LlmPreset::Codex
        } else {
            Self::infer_from_url(settings.llm_base_url.as_deref())
        }
    }

    /// The preset whose address matches; Custom when none does, LM Studio
    /// when nothing is stored yet.
    #[must_use]
    pub fn infer_from_url(url: Option<&str>) -> LlmPreset {
        let Some(url) = url else {
            return LlmPreset::LmStudio;
        };
        let normalized = normalize(url);
        LlmPreset::ALL
            .iter()
            .copied()
            .find(|preset| {
                preset
                    .base_url()
                    .is_some_and(|base| normalize(base) == normalized)
            })
            .unwrap_or(LlmPreset::Custom)
    }
}

fn normalize(url: &str) -> String {
    url.trim().to_lowercase().trim_end_matches('/').to_owned()
}

/// A base URL as typed, validated: http or https with a host.
/// Swift: `LLMSettingsViewModel.baseURL`.
#[must_use]
pub fn valid_base_url(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (scheme, rest) = trimmed.split_once("://")?;
    if !matches!(scheme.to_lowercase().as_str(), "http" | "https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| trimmed.to_owned())
}

/// The host of a valid base URL ("127.0.0.1").
#[must_use]
pub fn url_host(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_owned())
}

/// What the `ChatGPT` card knows about the sign-in. Swift: `CodexStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexStatus {
    NotChecked,
    SignedIn(String),
    Unavailable(String),
}

/// Swift: `LLMSettingsViewModel.TestResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestResult {
    Success(String),
    Failure(String),
}

/// The stored values, for `commit` to compare against. Swift: `Stored`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stored {
    provider: LlmProvider,
    base_url: Option<String>,
    model: Option<String>,
    context_tokens: i64,
    api_key: Option<String>,
    codex_model: Option<String>,
    codex_context_tokens: i64,
    codex_confirmed: bool,
}

#[derive(Debug)]
pub struct LlmSettingsViewModel {
    pub base_url_text: String,
    pub model: String,
    pub context_tokens_text: String,
    pub api_key: String,
    pub preset: LlmPreset,
    pub errors: SectionError,
    pub test_result: Option<TestResult>,
    pub is_testing: bool,
    pub is_configured: bool,
    pub codex_confirmed: bool,
    pub codex_status: CodexStatus,
    pub codex_models: Vec<CodexModel>,
    pub codex_models_error: Option<String>,
    pub is_loading_codex_models: bool,
    pub codex_model: String,
    codex_context_tokens: i64,
    stored: Option<Stored>,
}

impl LlmSettingsViewModel {
    pub const DEFAULT_CONTEXT_TOKENS: i64 = 32_000;

    #[must_use]
    pub fn new() -> Self {
        LlmSettingsViewModel {
            base_url_text: String::new(),
            model: String::new(),
            context_tokens_text: Self::DEFAULT_CONTEXT_TOKENS.to_string(),
            api_key: String::new(),
            preset: LlmPreset::LmStudio,
            errors: SectionError::default(),
            test_result: None,
            is_testing: false,
            is_configured: false,
            codex_confirmed: false,
            codex_status: CodexStatus::NotChecked,
            codex_models: Vec::new(),
            codex_models_error: None,
            is_loading_codex_models: false,
            codex_model: String::new(),
            codex_context_tokens: Settings::DEFAULT_CODEX_CONTEXT_TOKENS,
            stored: None,
        }
    }

    /// Whether a key is in the secret store, as last loaded or saved.
    #[must_use]
    pub fn has_stored_api_key(&self) -> bool {
        self.stored
            .as_ref()
            .and_then(|stored| stored.api_key.as_deref())
            .is_some_and(|key| !key.is_empty())
    }

    pub fn load(&mut self, store: &Store, services: &Services, secret: Option<String>) {
        let settings = match store.settings() {
            Ok(settings) => settings,
            Err(error) => {
                self.errors.fail("Settings could not be loaded.", error);
                return;
            }
        };
        self.preset = LlmPreset::infer_from_settings(&settings);
        let endpoint_preset = LlmPreset::infer_from_url(settings.llm_base_url.as_deref());
        self.base_url_text = settings
            .llm_base_url
            .clone()
            .or_else(|| endpoint_preset.base_url().map(str::to_owned))
            .unwrap_or_default();
        self.model = settings.llm_model.clone().unwrap_or_default();
        self.context_tokens_text = settings.llm_context_tokens.to_string();
        self.api_key = secret.clone().unwrap_or_default();
        self.codex_model = settings.codex_model.clone().unwrap_or_default();
        self.codex_context_tokens = settings.codex_context_tokens;
        self.codex_confirmed = settings.codex_confirmed_at.is_some();
        self.is_configured = llm_configured(&settings);
        self.stored = Some(Stored {
            provider: settings.llm_provider,
            base_url: settings.llm_base_url.clone(),
            model: settings.llm_model.clone(),
            context_tokens: settings.llm_context_tokens,
            api_key: secret,
            codex_model: settings.codex_model.clone(),
            codex_context_tokens: settings.codex_context_tokens,
            codex_confirmed: self.codex_confirmed,
        });
        // A fresh install shows the local preset's address without owning
        // it: opening and leaving the section must not write settings.
        if settings.llm_base_url.is_none() && settings.llm_provider == LlmProvider::Endpoint {
            self.stored = Some(self.draft());
        }
        if self.preset == LlmPreset::Codex {
            self.refresh_codex_status(services);
        }
    }

    // ChatGPT (Codex)

    /// Reads the sign-in, never the network.
    pub fn refresh_codex_status(&mut self, services: &Services) {
        self.codex_status = match services.llm.codex_account() {
            Ok(account) => CodexStatus::SignedIn(account),
            Err(reason) => CodexStatus::Unavailable(reason),
        };
    }

    /// The consent card's primary button: first the provider and the
    /// confirmation are saved (no model yet, so nothing is probed), then
    /// the model list is fetched, the first listed model picked, and the
    /// result saved and probed.
    pub fn confirm_codex(&mut self, store: &Store, services: &Services, now: DateTime<Utc>) {
        self.codex_confirmed = true;
        self.test_result = None;
        self.commit(store, services, now);
        if self.errors.error.is_some() {
            return;
        }
        self.refresh_codex_models(services);
        if self.codex_model.is_empty()
            && let Some(first) = self.codex_models.first()
        {
            self.codex_model.clone_from(&first.slug);
            self.codex_context_tokens = first
                .context_window
                .unwrap_or(Settings::DEFAULT_CODEX_CONTEXT_TOKENS);
        }
        self.commit(store, services, now);
    }

    /// "Stop using ChatGPT": clears the confirmation and returns to the
    /// endpoint provider with whatever it had.
    pub fn stop_using_codex(&mut self, store: &Store, services: &Services, now: DateTime<Utc>) {
        self.codex_confirmed = false;
        self.codex_models = Vec::new();
        self.codex_models_error = None;
        self.codex_status = CodexStatus::NotChecked;
        self.preset = LlmPreset::infer_from_url(self.base_url().as_deref());
        self.test_result = None;
        self.commit(store, services, now);
    }

    /// The backend's listed models. Only after confirmation.
    pub fn refresh_codex_models(&mut self, services: &Services) {
        if !self.codex_confirmed {
            return;
        }
        self.is_loading_codex_models = true;
        self.refresh_codex_status(services);
        match services.llm.codex_models() {
            Ok(models) => {
                self.codex_models = models;
                self.codex_models_error = None;
                if let Some(window) = self
                    .codex_models
                    .iter()
                    .find(|model| model.slug == self.codex_model)
                    .and_then(|model| model.context_window)
                {
                    self.codex_context_tokens = window;
                }
            }
            Err(CodexModelsError::Credential(reason)) => {
                self.codex_status = CodexStatus::Unavailable(reason);
                self.codex_models_error = None;
            }
            Err(CodexModelsError::Other(reason)) => {
                self.codex_models_error =
                    Some(format!("The model list could not be loaded. {reason}"));
            }
        }
        self.is_loading_codex_models = false;
    }

    /// The picker's choice: the slug and its context window, then save.
    pub fn select_codex_model(
        &mut self,
        slug: &str,
        store: &Store,
        services: &Services,
        now: DateTime<Utc>,
    ) {
        slug.clone_into(&mut self.codex_model);
        if let Some(window) = self
            .codex_models
            .iter()
            .find(|model| model.slug == slug)
            .and_then(|model| model.context_window)
        {
            self.codex_context_tokens = window;
        }
        self.test_result = None;
        self.commit(store, services, now);
    }

    // Draft

    /// The picker's rows: the listed models plus the stored slug when the
    /// list does not carry it.
    #[must_use]
    pub fn codex_model_choices(&self) -> Vec<CodexModel> {
        let mut choices = self.codex_models.clone();
        if !self.codex_model.is_empty()
            && !choices.iter().any(|model| model.slug == self.codex_model)
        {
            choices.push(CodexModel {
                slug: self.codex_model.clone(),
                display_name: self.codex_model.clone(),
                context_window: None,
            });
        }
        choices
    }

    /// The URL as typed, validated.
    #[must_use]
    pub fn base_url(&self) -> Option<String> {
        valid_base_url(&self.base_url_text)
    }

    #[must_use]
    pub fn context_tokens(&self) -> Option<i64> {
        self.context_tokens_text.trim().parse().ok()
    }

    #[must_use]
    pub fn validation_message(&self) -> Option<&'static str> {
        if self.preset == LlmPreset::Codex {
            return None;
        }
        if !self.base_url_text.trim().is_empty() && self.base_url().is_none() {
            return Some("The server address must start with http:// or https:// and name a host.");
        }
        if self.context_tokens().is_some_and(|tokens| tokens < 1_024) {
            return Some("The context size must be at least 1024.");
        }
        if self.context_tokens().is_none() && !self.context_tokens_text.is_empty() {
            return Some("The context size must be a number.");
        }
        None
    }

    fn draft(&self) -> Stored {
        let model = self.model.trim();
        let key = self.api_key.trim();
        Stored {
            provider: self.preset.provider(),
            // Under ChatGPT the server field is off screen; whatever it held stays.
            base_url: if self.preset == LlmPreset::Codex {
                self.base_url().or_else(|| {
                    self.stored
                        .as_ref()
                        .and_then(|stored| stored.base_url.clone())
                })
            } else {
                self.base_url()
            },
            model: (!model.is_empty()).then(|| model.to_owned()),
            context_tokens: self
                .context_tokens()
                .unwrap_or(Self::DEFAULT_CONTEXT_TOKENS),
            api_key: (!key.is_empty()).then(|| key.to_owned()),
            codex_model: (!self.codex_model.is_empty()).then(|| self.codex_model.clone()),
            codex_context_tokens: self.codex_context_tokens,
            codex_confirmed: self.codex_confirmed,
        }
    }

    /// A `Settings` with this draft's LLM fields, for the probe.
    fn settings_from(draft: &Stored, now: DateTime<Utc>) -> Settings {
        Settings {
            llm_provider: draft.provider,
            llm_base_url: draft.base_url.clone(),
            llm_model: draft.model.clone(),
            llm_context_tokens: draft.context_tokens,
            codex_model: draft.codex_model.clone(),
            codex_context_tokens: draft.codex_context_tokens,
            codex_confirmed_at: draft.codex_confirmed.then_some(now),
            ..Settings::default()
        }
    }

    // Actions

    /// Fills the address for the preset (Custom keeps what is typed) and
    /// stores it right away. `ChatGPT` stores nothing until the consent
    /// card's button.
    pub fn select_preset(
        &mut self,
        preset: LlmPreset,
        store: &Store,
        services: &Services,
        now: DateTime<Utc>,
    ) {
        self.preset = preset;
        if let Some(url) = preset.base_url() {
            url.clone_into(&mut self.base_url_text);
        }
        self.test_result = None;
        if preset == LlmPreset::Codex {
            self.refresh_codex_status(services);
            if self.codex_confirmed {
                self.commit(store, services, now);
            }
            return;
        }
        self.commit(store, services, now);
    }

    /// Saves when the form differs from what is stored and validates, then
    /// probes the endpoint when it is configured. Invalid input stays on
    /// screen as the validation message and saves nothing.
    pub fn commit(&mut self, store: &Store, services: &Services, now: DateTime<Utc>) {
        if self.validation_message().is_some() {
            return;
        }
        if self.preset == LlmPreset::Codex && !self.codex_confirmed {
            return;
        }
        if self.stored.as_ref() == Some(&self.draft()) {
            return;
        }
        self.save(store, services, now);
        if self.errors.error.is_none() && self.is_configured {
            self.test(services, now);
        }
    }

    pub fn save(&mut self, store: &Store, services: &Services, now: DateTime<Utc>) {
        if let Some(message) = self.validation_message() {
            self.errors.error = Some(message.to_owned());
            self.errors.details = None;
            return;
        }
        let draft = self.draft();
        let outcome = update_settings(store, |settings| {
            settings.llm_provider = draft.provider;
            settings.llm_base_url.clone_from(&draft.base_url);
            settings.llm_model.clone_from(&draft.model);
            settings.llm_context_tokens = draft.context_tokens;
            settings.codex_model.clone_from(&draft.codex_model);
            settings.codex_context_tokens = draft.codex_context_tokens;
            if draft.codex_confirmed {
                if settings.codex_confirmed_at.is_none() {
                    settings.codex_confirmed_at = Some(now);
                }
            } else {
                settings.codex_confirmed_at = None;
            }
        })
        .map_err(|error| error.to_string())
        .and_then(|settings| {
            crate::host::block_on(
                services
                    .secrets
                    .set_secret(&SecretKey::llm_api_key(), draft.api_key.as_deref()),
            )
            .map_err(|error| error.to_string())?;
            Ok(settings)
        })
        .and_then(|settings| {
            services.pipeline.reload()?;
            Ok(settings)
        });
        match outcome {
            Ok(settings) => {
                self.is_configured = llm_configured(&settings);
                self.stored = Some(draft);
                self.errors.clear();
            }
            Err(error) => self.errors.fail("Settings could not be saved.", error),
        }
    }

    /// Reachability, model listing and structured output mode, through the
    /// module's probe.
    pub fn test(&mut self, services: &Services, now: DateTime<Utc>) {
        let draft = self.draft();
        let missing = if draft.provider == LlmProvider::Codex {
            if !draft.codex_confirmed {
                Some("Confirm the use of your ChatGPT account first.")
            } else if draft.codex_model.is_none() {
                Some("Pick a model first.")
            } else {
                None
            }
        } else if draft.base_url.is_none() {
            Some("Enter a valid server address first.")
        } else if draft.model.is_none() {
            Some("Enter the model name first.")
        } else {
            None
        };
        if let Some(message) = missing {
            self.test_result = Some(TestResult::Failure(message.to_owned()));
            return;
        }
        self.is_testing = true;
        let probed = Self::settings_from(&draft, now);
        self.test_result = Some(
            match services.llm.probe(&probed, draft.api_key.as_deref()) {
                Ok(report) => TestResult::Success(report),
                Err(failure) => TestResult::Failure(failure),
            },
        );
        self.is_testing = false;
    }

    /// `settings.summaries.update`: the fields the page sent, as typed.
    pub fn apply_update(&mut self, update: &steno_bridge::SummariesUpdateParams) {
        if let Some(base_url) = &update.base_url {
            self.base_url_text.clone_from(base_url);
        }
        if let Some(model) = &update.model {
            self.model.clone_from(model);
        }
        if let Some(tokens) = &update.context_tokens {
            self.context_tokens_text.clone_from(tokens);
        }
        if let Some(key) = &update.api_key {
            self.api_key.clone_from(key);
        }
    }
}

impl Default for LlmSettingsViewModel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Swift: `SettingsSectionTests.testPresetsInferFromTheStoredAddress`.
    #[test]
    fn presets_infer_from_the_stored_address() {
        assert_eq!(LlmPreset::infer_from_url(None), LlmPreset::LmStudio);
        assert_eq!(
            LlmPreset::infer_from_url(Some("http://127.0.0.1:1234/v1/")),
            LlmPreset::LmStudio
        );
        assert_eq!(
            LlmPreset::infer_from_url(Some("HTTPS://api.openai.com/v1")),
            LlmPreset::OpenAi
        );
        assert_eq!(
            LlmPreset::infer_from_url(Some("http://gpu.local:8000/v1")),
            LlmPreset::Custom
        );
        let settings = Settings {
            llm_provider: LlmProvider::Codex,
            ..Settings::default()
        };
        assert_eq!(LlmPreset::infer_from_settings(&settings), LlmPreset::Codex);
    }

    #[test]
    fn base_urls_need_a_scheme_and_a_host() {
        assert_eq!(
            valid_base_url(" http://127.0.0.1:1234/v1 ").as_deref(),
            Some("http://127.0.0.1:1234/v1")
        );
        assert_eq!(valid_base_url("127.0.0.1:1234"), None);
        assert_eq!(valid_base_url("ftp://x/v1"), None);
        assert_eq!(valid_base_url("http:///v1"), None);
        assert_eq!(
            url_host("http://user@127.0.0.1:1234/v1").as_deref(),
            Some("127.0.0.1")
        );
    }
}
