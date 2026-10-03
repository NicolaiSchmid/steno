//! The LLM passes for the pipeline and the host's `LlmService`, built from
//! `Settings` and the API key: `LlmEndpoint::from_settings` (None until the
//! chosen provider is set up; the pipeline then skips both passes), one
//! client shared by the cleaner and the summarizer. The endpoint provider's
//! key goes into the client only; the Codex provider reads the Codex CLI's
//! sign-in. Swift: `apps/macos/Steno/Services/LLMWiring.swift`.

use std::sync::Arc;

use chrono_tz::Tz;
use steno_core::{MeetingSummarizer, Settings, TranscriptCleaner};
use steno_host::services::{CodexModel, CodexModelsError, LlmService};
use steno_llm::{
    CodexCredentialStore, CodexResponsesClient, LlmClient, LlmEndpoint, LlmMeetingSummarizer,
    LlmTranscriptCleaner, OpenAiCompatibleClient, RetryPolicy,
};

use crate::block_on;

/// The Codex CLI's sign-in, found through the process environment.
#[must_use]
pub fn codex_store() -> Arc<CodexCredentialStore> {
    Arc::new(CodexCredentialStore::new(
        CodexCredentialStore::default_home(&std::env::vars().collect()),
    ))
}

/// The two passes on one shared client.
pub struct Passes {
    pub cleaner: Arc<dyn TranscriptCleaner>,
    pub summarizer: Arc<dyn MeetingSummarizer>,
}

/// The client for `endpoint`: the Codex backend gets the credential store,
/// everything else the API key.
#[must_use]
pub fn make_client(
    endpoint: LlmEndpoint,
    api_key: Option<&str>,
    codex: &Arc<CodexCredentialStore>,
    retry: RetryPolicy,
) -> Arc<dyn LlmClient> {
    if endpoint.is_codex_backend() {
        Arc::new(CodexResponsesClient::new(endpoint, codex.clone()).with_retry(retry))
    } else {
        Arc::new(OpenAiCompatibleClient::new(endpoint, api_key).with_retry(retry))
    }
}

/// The real cleaner and summarizer, or `None` when the settings describe
/// no endpoint.
#[must_use]
pub fn passes(
    settings: &Settings,
    api_key: Option<&str>,
    codex: &Arc<CodexCredentialStore>,
    zone: Tz,
) -> Option<Passes> {
    let endpoint = LlmEndpoint::from_settings(settings)?;
    let client = make_client(endpoint.clone(), api_key, codex, RetryPolicy::default());
    let model: Arc<dyn steno_core::LanguageModel> = client;
    Some(Passes {
        cleaner: Arc::new(LlmTranscriptCleaner::new(model.clone(), endpoint.clone())),
        summarizer: Arc::new(LlmMeetingSummarizer::new(model, endpoint, zone)),
    })
}

/// One line for the Test button: whether `/models` lists the model, the
/// structured output mode the server accepted and the round trip.
pub async fn probe_line(
    settings: &Settings,
    api_key: Option<&str>,
    codex: &Arc<CodexCredentialStore>,
) -> Result<String, String> {
    let endpoint = LlmEndpoint::from_settings(settings)
        .ok_or_else(|| "Set up a summaries service first.".to_owned())?;
    // One attempt: a button press should answer at once, not after the
    // pipeline's retry backoff.
    let client = make_client(endpoint, api_key, codex, RetryPolicy::with_max_attempts(1));
    let report = client.probe().await.map_err(|error| error.to_string())?;
    let listed = match report.model_listed {
        Some(true) => "model listed",
        Some(false) => "model not in /models",
        None => "no model list",
    };
    let who = report
        .account_line
        .as_deref()
        .map(|line| format!(" as {line}"))
        .unwrap_or_default();
    let mode = serde_json::to_value(report.resolved_mode)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    Ok(format!(
        "Connected{who}: {listed}, structured output {mode}, {} ms.",
        report.round_trip.as_millis()
    ))
}

/// The host's `LlmService` over the real clients; blocks on the runtime
/// handle the app runs on (the host's traits are synchronous).
pub struct ClientLlmService {
    pub codex: Arc<CodexCredentialStore>,
    pub runtime: tokio::runtime::Handle,
}

impl LlmService for ClientLlmService {
    fn probe(&self, settings: &Settings, api_key: Option<&str>) -> Result<String, String> {
        block_on(&self.runtime, probe_line(settings, api_key, &self.codex))
    }

    fn codex_account(&self) -> Result<String, String> {
        block_on(&self.runtime, self.codex.current())
            .map(|credentials| credentials.account_line())
            .map_err(|error| error.to_string())
    }

    fn codex_models(&self) -> Result<Vec<CodexModel>, CodexModelsError> {
        block_on(&self.runtime, async {
            self.codex
                .current()
                .await
                .map_err(|error| CodexModelsError::Credential(error.to_string()))?;
            let client = CodexResponsesClient::new(
                LlmEndpoint::codex("list", Settings::DEFAULT_CODEX_CONTEXT_TOKENS),
                self.codex.clone(),
            )
            .with_retry(RetryPolicy::with_max_attempts(1));
            let models = client
                .list_models()
                .await
                .map_err(|error| CodexModelsError::Other(error.to_string()))?;
            Ok(models
                .into_iter()
                .filter(steno_llm::CodexModel::is_listed)
                .map(|model| CodexModel {
                    slug: model.slug,
                    display_name: model.display_name,
                    context_window: model.context_window,
                })
                .collect())
        })
    }
}
