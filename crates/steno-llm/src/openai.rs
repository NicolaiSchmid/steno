//! The endpoint client: `POST {base}/chat/completions` with Bearer auth.
//! Swift: `Sources/StenoLLM/OpenAICompatibleClient.swift`.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use steno_core::{
    BoundaryResult, LanguageModel, LlmFinishReason, LlmMessage, LlmRequest, LlmResponse,
    LlmResponseFormat, LlmResponseFormatKind, LlmRole, LlmUsage, SecretKey, SecretStore,
    async_trait,
};

use crate::endpoint::{RememberedMode, WireFormat};
use crate::transport::{self, Attempt, HttpReply, LlmClientEvent, Observer, notify};
use crate::wire::{
    self, ChatCompletionRequest, ChatCompletionResponse, ChatErrorEnvelope, ChatResponseFormat,
    ModelList,
};
use crate::{
    Clock, EndpointProbe, JsonSchema, LlmClient, LlmEndpoint, LlmError, RetryPolicy,
    StructuredOutputMode, SystemClock,
};

/// The endpoint [`LlmClient`]: `POST {base}/chat/completions` with Bearer
/// auth, a per-attempt timeout and exponential retries on the injected
/// clock, structured output mode fallback remembered per endpoint, and the
/// API key redacted from every error and from the `Debug` form. Text only
/// ever leaves through here or
/// [`CodexResponsesClient`](crate::CodexResponsesClient).
pub struct OpenAiCompatibleClient {
    endpoint: LlmEndpoint,
    api_key: Option<String>,
    /// The error instead when the default client could not be built.
    http: Result<reqwest::Client, LlmError>,
    retry: RetryPolicy,
    clock: Arc<dyn Clock>,
    observer: Option<Observer>,
    mode: RememberedMode,
    /// Parameters a 400 named (`error.param`) and that the client now
    /// spells differently, remembered per client like the mode:
    /// `max_tokens` goes as `max_completion_tokens` (OpenAI's reasoning
    /// models), `temperature` is left out (they accept only the default).
    /// No model-name sniffing.
    rejected_parameters: Mutex<BTreeSet<String>>,
}

impl fmt::Debug for OpenAiCompatibleClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiCompatibleClient")
            .field("endpoint", &self.endpoint)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field("retry", &self.retry)
            .field("mode", &self.mode)
            .field("rejected_parameters", &*self.rejected_parameters())
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatibleClient {
    /// A client with the default HTTP client, retry policy and system
    /// clock. An empty key is no key.
    ///
    /// ```
    /// use steno_llm::{LlmClient, LlmEndpoint, OpenAiCompatibleClient};
    /// use url::Url;
    ///
    /// let endpoint = LlmEndpoint::new(Url::parse("http://127.0.0.1:1234/v1")?, "qwen3");
    /// let client = OpenAiCompatibleClient::new(endpoint, None);
    /// assert_eq!(client.endpoint().model, "qwen3");
    /// # Ok::<(), url::ParseError>(())
    /// ```
    #[must_use]
    pub fn new(endpoint: LlmEndpoint, api_key: Option<&str>) -> Self {
        let mode = endpoint.structured_output_mode;
        OpenAiCompatibleClient {
            endpoint,
            api_key: api_key.filter(|key| !key.is_empty()).map(str::to_owned),
            http: transport::default_http_client(),
            retry: RetryPolicy::default(),
            clock: Arc::new(SystemClock::default()),
            observer: None,
            mode: RememberedMode::new(mode),
            rejected_parameters: Mutex::new(BTreeSet::new()),
        }
    }

    /// A client whose key is the [`SecretKey::LLM_API_KEY`] secret of
    /// `secrets`; a missing secret is no key, as for a local server.
    pub async fn from_secret_store(
        endpoint: LlmEndpoint,
        secrets: &dyn SecretStore,
    ) -> BoundaryResult<Self> {
        let key = secrets.secret(&SecretKey::llm_api_key()).await?;
        Ok(Self::new(endpoint, key.as_deref()))
    }

    /// An HTTP client of the caller's, for a proxy or for connect and read
    /// timeouts. Its overall `ClientBuilder::timeout` does not apply: every
    /// request carries its own, the wall-clock backstop past
    /// `endpoint.request_timeout`, and reqwest prefers that. Start it from
    /// [`transport::http_client_builder`]: reqwest comes without a TLS
    /// provider here, and that builder installs one.
    #[must_use]
    pub fn with_http(mut self, http: reqwest::Client) -> Self {
        self.http = Ok(http);
        self
    }

    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    #[must_use]
    pub fn with_observer(mut self, observer: Observer) -> Self {
        self.observer = Some(observer);
        self
    }

    fn secrets(&self) -> &[String] {
        self.api_key.as_slice()
    }

    fn rejected_parameters(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.rejected_parameters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// One completion with this crate's own error type; the
    /// [`LanguageModel`] impl boxes it.
    pub async fn complete_llm(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError> {
        transport::run_attempts(
            &self.retry,
            self.clock.as_ref(),
            self.observer.as_ref(),
            move |attempt| self.attempt(request, attempt),
        )
        .await
    }

    /// One attempt: the parsed answer or the failure to back off from, or a
    /// resend after a mode downgrade or a parameter adjustment (neither
    /// consumes an attempt). A request that cannot be built is final.
    async fn attempt(&self, request: &LlmRequest, attempt: u32) -> Result<Attempt, LlmError> {
        let mode = self.mode.get();
        let adjusted = self.rejected_parameters().clone();
        let wire = self.make_request(request, mode, &adjusted)?;
        notify(
            self.observer.as_ref(),
            LlmClientEvent::Request {
                attempt,
                purpose: request.purpose.clone(),
                mode,
            },
        );
        let reply = match self.perform(wire).await {
            Ok(reply) => reply,
            Err(failure) => return Ok(Attempt::Failed(failure)),
        };
        notify(
            self.observer.as_ref(),
            LlmClientEvent::Response {
                attempt,
                status: reply.status,
            },
        );
        if reply.is_success() {
            return Ok(match self.parse(&reply) {
                Ok(response) => Attempt::Done(response),
                Err(failure) => Attempt::Failed(failure),
            });
        }
        if reply.status == 400
            && request.response_format.kind() != LlmResponseFormatKind::Text
            && Self::complains_about_response_format(&reply.body_text())
            && self.mode.step_down_from(mode, self.observer.as_ref())
        {
            return Ok(Attempt::Resend);
        }
        // Resent whenever this request still used the old spelling, even if
        // a concurrent request already taught the client the new one; only
        // the request that taught it reports it. The rebuilt request never
        // carries the parameter again, so this resends at most once per
        // parameter.
        if reply.status == 400
            && let Some(param) = Self::rejected_parameter(&reply)
            && Self::ADJUSTABLE_PARAMETERS.contains(&param.as_str())
            && !adjusted.contains(&param)
        {
            if self.rejected_parameters().insert(param.clone()) {
                notify(
                    self.observer.as_ref(),
                    LlmClientEvent::ParameterRejected(param),
                );
            }
            return Ok(Attempt::Resend);
        }
        Ok(Attempt::Failed(self.classify(&reply)))
    }

    /// `GET /models` (whether the model is listed; a server without a list
    /// is tolerated) and one tiny structured completion (mode fallback,
    /// round trip). Any failure of the completion is returned, so a probe
    /// that succeeds describes an endpoint both passes can use; a wrong
    /// base URL, an unknown model or an undecodable answer is the caller's
    /// to show.
    pub async fn probe_llm(&self) -> Result<EndpointProbe, LlmError> {
        let start = self.clock.now();
        let mut model_listed = None;
        let models = transport::client(&self.http)?
            .get(self.endpoint.models_url())
            .headers(self.headers("probe"))
            .build()
            .map_err(|error| LlmError::Transport(error.to_string()))?;
        if let Ok(reply) = self.perform(models).await
            && reply.is_success()
            && let Ok(list) = wire::decode::<ModelList>(&reply.body)
        {
            model_listed = Some(
                list.data
                    .iter()
                    .any(|model| model.id == self.endpoint.model),
            );
        }
        self.complete_llm(&Self::probe_request()).await?;
        Ok(EndpointProbe {
            model_listed,
            resolved_mode: self.mode.get(),
            round_trip: self.clock.now().saturating_sub(start),
            account_line: None,
        })
    }

    /// Through the same builder as every other schema, so the strict-subset
    /// walk in the tests covers it.
    #[must_use]
    pub fn probe_schema() -> JsonSchema {
        JsonSchema::object(vec![("ok", JsonSchema::boolean())])
    }

    #[must_use]
    pub fn probe_request() -> LlmRequest {
        LlmRequest {
            messages: vec![
                LlmMessage {
                    role: LlmRole::System,
                    content: "Reply with JSON only.".to_owned(),
                },
                LlmMessage {
                    role: LlmRole::User,
                    content: "Return exactly {\"ok\": true}.".to_owned(),
                },
            ],
            response_format: LlmResponseFormat::JsonSchema {
                name: "probe".to_owned(),
                schema: Self::probe_schema().json_value(),
                strict: true,
            },
            temperature: Some(0.0),
            max_tokens: Some(32),
            purpose: "probe".to_owned(),
        }
    }

    // Requests

    /// The wire request under `mode`, built around the `adjusted`
    /// (rejected) parameters: a 400 naming one of those is final, not a
    /// resend.
    fn make_request(
        &self,
        request: &LlmRequest,
        mode: StructuredOutputMode,
        adjusted: &BTreeSet<String>,
    ) -> Result<reqwest::Request, LlmError> {
        let ceiling = request
            .max_tokens
            .unwrap_or(self.endpoint.max_output_tokens);
        let renames_max_tokens = adjusted.contains("max_tokens");
        let drops_temperature = adjusted.contains("temperature");
        let body = ChatCompletionRequest {
            model: self.endpoint.model.clone(),
            messages: request.messages.iter().map(Into::into).collect(),
            temperature: if drops_temperature {
                None
            } else {
                request.temperature
            },
            max_tokens: (!renames_max_tokens).then_some(ceiling),
            max_completion_tokens: renames_max_tokens.then_some(ceiling),
            response_format: Self::response_format(&request.response_format, mode),
        };
        let bytes = wire::encode(&body).map_err(|error| LlmError::Transport(error.to_string()))?;
        transport::client(&self.http)?
            .post(self.endpoint.chat_completions_url())
            .headers(self.headers(&request.purpose))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes)
            .build()
            .map_err(|error| LlmError::Transport(error.to_string()))
    }

    fn headers(&self, purpose: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT,
            "application/json".parse().expect("ascii"),
        );
        headers.insert(
            reqwest::header::USER_AGENT,
            crate::USER_AGENT.parse().expect("ascii"),
        );
        if let Ok(value) = purpose.parse() {
            headers.insert("X-Steno-Purpose", value);
        }
        if let Some(key) = &self.api_key
            && let Ok(mut value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
        {
            // Sensitive: the `Debug` form of the headers and of any request
            // built from them prints `Sensitive`, not the key.
            value.set_sensitive(true);
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        headers
    }

    /// The wire `response_format` for a request under `mode`; `None` sends
    /// none.
    #[must_use]
    pub fn response_format(
        format: &LlmResponseFormat,
        mode: StructuredOutputMode,
    ) -> Option<ChatResponseFormat> {
        Some(match mode.wire_format(format)? {
            WireFormat::JsonObject => ChatResponseFormat::json_object(),
            WireFormat::JsonSchema {
                name,
                schema,
                strict,
            } => ChatResponseFormat::json_schema(name, schema.clone(), strict),
        })
    }

    /// One attempt raced against `endpoint.request_timeout` on the clock.
    async fn perform(&self, request: reqwest::Request) -> Result<HttpReply, LlmError> {
        transport::perform(
            transport::client(&self.http)?,
            request,
            self.clock.as_ref(),
            self.endpoint.request_timeout,
            self.secrets(),
        )
        .await
    }

    // Replies

    fn parse(&self, reply: &HttpReply) -> Result<LlmResponse, LlmError> {
        let decoded: ChatCompletionResponse = wire::decode(&reply.body).map_err(|_| {
            LlmError::Transport(format!(
                "undecodable completion body: {}",
                reply.redacted_text(self.secrets(), 4_096)
            ))
        })?;
        let choice = decoded
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::Transport("completion without choices".to_owned()))?;
        if let Some(refusal) = &choice.message.refusal
            && !refusal.is_empty()
        {
            return Err(LlmError::Refused(self.redact(refusal)));
        }
        let finish_reason = match choice.finish_reason.as_deref() {
            Some("stop") => LlmFinishReason::Stop,
            Some("length") => LlmFinishReason::Length,
            Some("content_filter") => LlmFinishReason::ContentFilter,
            _ => LlmFinishReason::Other,
        };
        let usage = decoded.usage.unwrap_or_default();
        Ok(LlmResponse {
            text: choice.message.content.unwrap_or_default(),
            finish_reason,
            usage: Some(LlmUsage {
                prompt_tokens: usage.prompt_tokens.unwrap_or(0),
                completion_tokens: usage.completion_tokens.unwrap_or(0),
                requests: 1,
            }),
            model: decoded.model,
        })
    }

    fn classify(&self, reply: &HttpReply) -> LlmError {
        if reply.status == 429 {
            return transport::rate_limited(reply);
        }
        LlmError::Http {
            status: reply.status,
            body: Self::error_message(reply, self.secrets()),
        }
    }

    /// The server's `error.message`, else the first 500 characters of the
    /// body, with every secret redacted.
    #[must_use]
    pub fn error_message(reply: &HttpReply, secrets: &[String]) -> String {
        if let Ok(envelope) = wire::decode::<ChatErrorEnvelope>(&reply.body) {
            return transport::redact(&envelope.error.message, secrets);
        }
        reply.redacted_text(secrets, 500)
    }

    /// A 400 whose message names the structured output request: the cue to
    /// fall back one mode. Groq, OpenRouter and OpenAI all word it this way.
    #[must_use]
    pub fn complains_about_response_format(body: &str) -> bool {
        let lowered = body.to_lowercase();
        lowered.contains("response_format")
            || lowered.contains("json_schema")
            || lowered.contains("json schema")
            || lowered.contains("structured output")
    }

    /// The parameters a 400 may name that the client can spell differently.
    pub const ADJUSTABLE_PARAMETERS: [&'static str; 2] = ["max_tokens", "temperature"];

    /// `error.param` of a 400 envelope, when the server sent one.
    #[must_use]
    pub fn rejected_parameter(reply: &HttpReply) -> Option<String> {
        wire::decode::<ChatErrorEnvelope>(&reply.body)
            .ok()?
            .error
            .param
    }

    fn redact(&self, text: &str) -> String {
        transport::redact(text, self.secrets())
    }
}

#[async_trait]
impl LanguageModel for OpenAiCompatibleClient {
    async fn complete(&self, request: &LlmRequest) -> BoundaryResult<LlmResponse> {
        Ok(self.complete_llm(request).await?)
    }
}

#[async_trait]
impl LlmClient for OpenAiCompatibleClient {
    fn endpoint(&self) -> &LlmEndpoint {
        &self.endpoint
    }

    fn resolved_mode(&self) -> StructuredOutputMode {
        self.mode.get()
    }

    async fn probe(&self) -> BoundaryResult<EndpointProbe> {
        Ok(self.probe_llm().await?)
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;

    fn client() -> OpenAiCompatibleClient {
        let endpoint = LlmEndpoint::new(Url::parse("http://127.0.0.1:9/v1").unwrap(), "m");
        OpenAiCompatibleClient::new(endpoint, Some("sk-unit-secret"))
    }

    #[test]
    fn the_authorization_header_is_sensitive() {
        let client = client();
        let headers = client.headers("test");
        assert_eq!(
            headers[reqwest::header::AUTHORIZATION].to_str().unwrap(),
            "Bearer sk-unit-secret"
        );
        let debug = format!("{headers:?}");
        assert!(!debug.contains("sk-unit-secret"), "{debug}");
        assert!(debug.contains("Sensitive"), "{debug}");
    }

    /// A client whose HTTP client could not be built answers every call
    /// with that failure, final, and never panics.
    #[tokio::test]
    async fn without_an_http_client_every_call_fails_with_the_reason() {
        let mut client = client();
        let reason = transport::http_client_unavailable(
            "builder error: No CA certificates were loaded from the system",
        );
        assert_eq!(
            reason.to_string(),
            "no trusted root certificates were found on this computer"
        );
        assert!(!reason.is_retryable());
        client.http = Err(reason.clone());
        let request = LlmRequest {
            messages: Vec::new(),
            response_format: steno_core::LlmResponseFormat::Text,
            temperature: None,
            max_tokens: None,
            purpose: "test".to_owned(),
        };
        assert_eq!(client.complete_llm(&request).await.unwrap_err(), reason);
        assert_eq!(client.probe_llm().await.unwrap_err(), reason);
        assert_eq!(
            transport::http_client_unavailable("builder error: bad proxy").to_string(),
            "the HTTP client could not be built: builder error: bad proxy"
        );
    }

    #[test]
    fn the_debug_form_redacts_the_api_key() {
        let debug = format!("{:?}", client());
        assert!(!debug.contains("sk-unit-secret"), "{debug}");
        assert!(debug.contains("[redacted]"), "{debug}");
        assert!(
            debug.contains("127.0.0.1"),
            "the endpoint stays visible: {debug}"
        );
    }
}
