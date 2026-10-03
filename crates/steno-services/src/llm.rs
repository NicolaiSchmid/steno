//! The LLM passes for the pipeline and the host's `LlmService`, built from
//! `Settings` and the API key: `LlmEndpoint::from_settings` (None until the
//! chosen provider is set up; the pipeline then skips both passes), one
//! client shared by the cleaner and the summarizer. The endpoint provider's
//! key goes into the client only; the Codex provider reads the Codex CLI's
//! sign-in. Swift: `apps/macos/Steno/Services/LLMWiring.swift`.

use std::future::Future;
use std::sync::{Arc, OnceLock};

use chrono_tz::Tz;
use steno_core::protocols::BoundaryResult;
use steno_core::{MeetingSummarizer, Settings, TranscriptCleaner};
use steno_host::services::{CodexModel, CodexModelsError, LlmService};
use steno_llm::{
    CodexCredentialStore, CodexResponsesClient, LlmClient, LlmEndpoint, LlmMeetingSummarizer,
    LlmTranscriptCleaner, OpenAiCompatibleClient, RetryPolicy,
};

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

/// The runtime the host's network calls run on, with a worker of its
/// own. The host calls in from a bridge command, which can itself be
/// running on an app runtime worker, and the app runtime's other workers
/// can all be waiting on the host's state (the event loop and the poll
/// take it), leaving nobody there to drive a request or fire its timeout.
/// It lives as long as the process, because a pooled connection it opens
/// may later serve the pipeline's client and would break if this runtime
/// were dropped.
fn network_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("steno-network")
            .enable_all()
            .build()
            .expect("the network runtime")
    })
}

/// Runs `future` on [`network_runtime`] and waits for it; `None` when it
/// panicked.
fn on_network_runtime<T: Send + 'static>(
    future: impl Future<Output = T> + Send + 'static,
) -> Option<T> {
    let (sender, receiver) = std::sync::mpsc::channel();
    network_runtime().spawn(async move {
        let _ = sender.send(future.await);
    });
    let wait = move || receiver.recv().ok();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(wait)
        }
        _ => wait(),
    }
}

const NETWORK_CALL_PANICKED: &str = "The request stopped unexpectedly.";

/// The host's `LlmService` over the real clients. The host's traits are
/// synchronous, so the probe and the model list wait on the network
/// runtime; the host calls both with its lock released (the probe's single
/// attempt and the clients' timeouts bound the wait). The sign-in is read
/// from the file, never refreshed over the network, as Swift's
/// `refreshCodexStatus` read `CodexCredentialStore.stored()`.
pub struct ClientLlmService {
    pub codex: Arc<CodexCredentialStore>,
}

impl LlmService for ClientLlmService {
    fn probe(&self, settings: &Settings, api_key: Option<&str>) -> BoundaryResult<String> {
        let (settings, api_key, codex) = (
            settings.clone(),
            api_key.map(str::to_owned),
            self.codex.clone(),
        );
        Ok(on_network_runtime(
            async move { probe_line(&settings, api_key.as_deref(), &codex).await },
        )
        .unwrap_or_else(|| Err(NETWORK_CALL_PANICKED.to_owned()))?)
    }

    fn codex_account(&self) -> BoundaryResult<String> {
        Ok(self.codex.stored()?.account_line())
    }

    fn codex_models(&self) -> Result<Vec<CodexModel>, CodexModelsError> {
        let codex = self.codex.clone();
        on_network_runtime(async move {
            codex
                .current()
                .await
                .map_err(|error| CodexModelsError::Credential(error.to_string()))?;
            let client = CodexResponsesClient::new(
                LlmEndpoint::codex("list", Settings::DEFAULT_CODEX_CONTEXT_TOKENS),
                codex,
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
        .unwrap_or_else(|| Err(CodexModelsError::Other(NETWORK_CALL_PANICKED.to_owned())))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use steno_core::LlmProvider;
    use steno_llm::testing::{Scripts, StubChatServer};

    use super::*;
    use crate::testing::on_own_thread;

    /// The deadlock the network runtime prevents: the app runtime's only
    /// worker is parked on a lock the caller holds (as the host's flush
    /// timer parks on the host's state while a Settings command holds it),
    /// and the probe still answers.
    #[test]
    fn a_probe_answers_while_every_app_worker_waits_on_a_lock_the_caller_holds() {
        let one_worker = || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap()
        };
        let (app, elsewhere) = (one_worker(), one_worker());
        let server = elsewhere.block_on(StubChatServer::start()).unwrap();
        server.respond(Arc::new(|request| {
            if request.path.ends_with("/models") {
                Some(Scripts.models(&["stub-model"]))
            } else {
                Some(Scripts.json(&serde_json::json!({ "ok": true }), None))
            }
        }));
        let settings = Settings {
            llm_provider: LlmProvider::Endpoint,
            llm_base_url: Some(server.base_url().to_string()),
            llm_model: Some("stub-model".to_owned()),
            ..Settings::default()
        };

        let host_state = Arc::new(Mutex::new(()));
        let held = host_state.lock().unwrap();
        let parked = host_state.clone();
        let (parked_sender, parked_receiver) = std::sync::mpsc::channel();
        app.spawn(async move {
            parked_sender.send(()).unwrap();
            drop(parked.lock().unwrap());
        });
        parked_receiver.recv().unwrap();

        let service = ClientLlmService {
            codex: codex_store(),
        };
        let answer = on_own_thread(
            Duration::from_secs(10),
            "the probe answered while the app runtime was parked",
            move || service.probe(&settings, None),
        );
        drop(held);
        let line = answer.unwrap();
        assert!(
            line.starts_with("Connected: model listed, structured output"),
            "{line}"
        );
    }

    /// The Summaries section reads the sign-in with the host's lock held,
    /// so the account line comes from the file even when its tokens are
    /// due for a refresh: no request reaches the token endpoint.
    #[test]
    fn the_codex_account_is_read_from_the_file_without_a_refresh() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(StubChatServer::start()).unwrap();
        let home = tempfile::tempdir().unwrap();
        // No `last_refresh`: the file counts as stale, so `current()` would
        // refresh first.
        std::fs::write(
            home.path().join("auth.json"),
            r#"{"OPENAI_API_KEY":null,"tokens":{"access_token":"access","refresh_token":"refresh","account_id":"account"}}"#,
        )
        .unwrap();
        let service = ClientLlmService {
            codex: Arc::new(
                CodexCredentialStore::new(home.path())
                    .with_token_endpoint(server.base_url().join("oauth/token").unwrap()),
            ),
        };
        assert_eq!(service.codex_account().unwrap(), "your ChatGPT account");
        assert!(server.requests().is_empty(), "{:?}", server.requests());
    }
}
