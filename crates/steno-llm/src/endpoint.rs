//! Where the model lives, how much it holds, how JSON is asked for, and the
//! trait both clients implement.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use steno_core::{
    BoundaryResult, LanguageModel, LlmProvider, LlmResponseFormat, Settings, async_trait,
    string_enum,
};
use url::Url;

use crate::transport::{LlmClientEvent, Observer, notify};

string_enum! {
    /// How the client asks for JSON, falling back on a 400 that names
    /// `response_format`: `JsonSchema` to `JsonObject` to `PromptOnly`. The
    /// schema is always in the prompt as well.
    pub enum StructuredOutputMode {
        JsonSchema = "jsonSchema",
        JsonObject = "jsonObject",
        PromptOnly = "promptOnly",
    }
}

impl StructuredOutputMode {
    /// The next weaker mode; `None` from `PromptOnly`.
    #[must_use]
    pub fn downgraded(self) -> Option<StructuredOutputMode> {
        match self {
            StructuredOutputMode::JsonSchema => Some(StructuredOutputMode::JsonObject),
            StructuredOutputMode::JsonObject => Some(StructuredOutputMode::PromptOnly),
            StructuredOutputMode::PromptOnly => None,
        }
    }

    /// What goes on the wire for `format` under this mode: nothing for text
    /// or `PromptOnly`, `json_object`, or the schema itself.
    pub(crate) fn wire_format(self, format: &LlmResponseFormat) -> Option<WireFormat<'_>> {
        match (format, self) {
            (LlmResponseFormat::Text, _) | (_, StructuredOutputMode::PromptOnly) => None,
            (LlmResponseFormat::JsonObject, _)
            | (LlmResponseFormat::JsonSchema { .. }, StructuredOutputMode::JsonObject) => {
                Some(WireFormat::JsonObject)
            }
            (
                LlmResponseFormat::JsonSchema {
                    name,
                    schema,
                    strict,
                },
                StructuredOutputMode::JsonSchema,
            ) => Some(WireFormat::JsonSchema {
                name,
                schema,
                strict: *strict,
            }),
        }
    }
}

/// The structured output mode a client remembers: it starts at the
/// endpoint's and only ever steps down.
#[derive(Debug)]
pub(crate) struct RememberedMode(Mutex<StructuredOutputMode>);

impl RememberedMode {
    pub(crate) fn new(mode: StructuredOutputMode) -> Self {
        RememberedMode(Mutex::new(mode))
    }

    pub(crate) fn get(&self) -> StructuredOutputMode {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether a weaker mode than `sent` exists, stepping to it only while
    /// the remembered mode is still `sent`, so a late 400 from a concurrent
    /// request neither bounces the mode back up nor announces the step twice.
    pub(crate) fn step_down_from(
        &self,
        sent: StructuredOutputMode,
        observer: Option<&Observer>,
    ) -> bool {
        let Some(next) = sent.downgraded() else {
            return false;
        };
        let moved = {
            let mut mode = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            if *mode == sent {
                *mode = next;
                true
            } else {
                false
            }
        };
        if moved {
            notify(observer, LlmClientEvent::ModeDowngraded { to: next });
        }
        true
    }
}

/// The structured output request after the mode fallback.
pub(crate) enum WireFormat<'a> {
    JsonObject,
    JsonSchema {
        name: &'a str,
        schema: &'a serde_json::Value,
        strict: bool,
    },
}

/// Where the model lives and how much it can hold. Not serialisable: the
/// API key is never part of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmEndpoint {
    /// The OpenAI-compatible root, `http://127.0.0.1:1234/v1` for LM Studio.
    pub base_url: Url,
    pub model: String,
    /// Input budgets derive from it.
    pub context_tokens: i64,
    pub max_output_tokens: i64,
    pub max_concurrent_requests: usize,
    /// Per attempt, on the injected clock.
    pub request_timeout: Duration,
    pub structured_output_mode: StructuredOutputMode,
    /// Only [`LlmEndpoint::codex`] sets `Codex`, so the confirmation gate
    /// cannot be walked around by pasting the Codex URL.
    pub provider: LlmProvider,
}

impl LlmEndpoint {
    /// OpenAI's Codex backend; the client appends `/responses` and `/models`.
    pub const CODEX_BACKEND_URL: &'static str = "https://chatgpt.com/backend-api/codex";

    pub const MINIMUM_CONTEXT_TOKENS: i64 = 1_024;

    /// An endpoint with the defaults: 32k context, 4 096 output tokens, two
    /// requests in flight, 240 s per attempt, `JsonSchema`.
    #[must_use]
    pub fn new(base_url: Url, model: impl Into<String>) -> Self {
        LlmEndpoint {
            base_url,
            model: model.into(),
            context_tokens: 32_000,
            max_output_tokens: 4_096,
            max_concurrent_requests: 2,
            request_timeout: Duration::from_secs(240),
            structured_output_mode: StructuredOutputMode::JsonSchema,
            provider: LlmProvider::Endpoint,
        }
    }

    /// The Codex backend with `model`. The backend takes no output ceiling,
    /// so `max_output_tokens` only shapes prompts and budgets.
    #[must_use]
    pub fn codex(model: impl Into<String>, context_tokens: i64) -> Self {
        let base_url = Url::parse(Self::CODEX_BACKEND_URL).expect("a literal URL");
        LlmEndpoint {
            context_tokens: context_tokens.max(Self::MINIMUM_CONTEXT_TOKENS),
            max_output_tokens: 16_000,
            provider: LlmProvider::Codex,
            ..LlmEndpoint::new(base_url, model)
        }
    }

    /// The endpoint the settings describe, or `None` while summaries are off
    /// or not set up: an endpoint needs a parsing URL and a model, Codex a
    /// model and the confirmed credential use.
    #[must_use]
    pub fn from_settings(settings: &Settings) -> Option<Self> {
        match settings.llm_provider {
            LlmProvider::Endpoint => {
                let base_url = Url::parse(settings.llm_base_url.as_deref()?).ok()?;
                let model = settings.llm_model.as_deref().filter(|m| !m.is_empty())?;
                Some(LlmEndpoint {
                    context_tokens: settings
                        .llm_context_tokens
                        .max(Self::MINIMUM_CONTEXT_TOKENS),
                    ..LlmEndpoint::new(base_url, model)
                })
            }
            LlmProvider::Codex => {
                settings.codex_confirmed_at?;
                let model = settings.codex_model.as_deref().filter(|m| !m.is_empty())?;
                Some(LlmEndpoint::codex(model, settings.codex_context_tokens))
            }
        }
    }

    #[must_use]
    pub fn is_codex_backend(&self) -> bool {
        self.provider == LlmProvider::Codex
    }

    /// `base_url` with `path` appended, a trailing slash tolerated.
    #[must_use]
    pub fn url(&self, path: &str) -> Url {
        let mut url = self.base_url.clone();
        let base = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{base}/{path}"));
        url.set_query(None);
        url
    }

    #[must_use]
    pub fn chat_completions_url(&self) -> Url {
        self.url("chat/completions")
    }

    #[must_use]
    pub fn responses_url(&self) -> Url {
        self.url("responses")
    }

    #[must_use]
    pub fn models_url(&self) -> Url {
        self.url("models")
    }
}

/// What a successful `probe()` learned about an endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointProbe {
    /// Whether `GET /models` lists the model; `None` without a model list.
    pub model_listed: Option<bool>,
    pub resolved_mode: StructuredOutputMode,
    pub round_trip: Duration,
    /// "name@example.com (Plus)" from the Codex sign-in.
    pub account_line: Option<String>,
}

/// A [`LanguageModel`] bound to one [`LlmEndpoint`] that can probe its own
/// setup without a downcast.
#[async_trait]
pub trait LlmClient: LanguageModel {
    fn endpoint(&self) -> &LlmEndpoint;

    fn resolved_mode(&self) -> StructuredOutputMode;

    async fn probe(&self) -> BoundaryResult<EndpointProbe>;
}
