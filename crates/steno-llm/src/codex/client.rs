//! The Codex client: the Responses API on OpenAI's Codex backend with the
//! ChatGPT sign-in from the credential store.
//! Swift: `Sources/StenoLLM/Codex/CodexResponsesClient.swift`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use steno_core::{
    BoundaryResult, BoxError, LanguageModel, LlmFinishReason, LlmRequest, LlmResponse,
    LlmResponseFormat, LlmResponseFormatKind, LlmRole, LlmUsage, async_trait,
};

use super::{CodexCredentialError, CodexCredentialStore, CodexCredentials};
use crate::endpoint::WireFormat;
use crate::transport::{self, Attempt, HttpReply, LlmClientEvent, Observer, notify};
use crate::wire::{
    self, CodexErrorEnvelope, CodexModel, CodexModelList, ResponsesFormat, ResponsesOutputItem,
    ResponsesReasoning, ResponsesRequest, ResponsesResponse, ResponsesStreamEvent, ResponsesText,
};
use crate::{
    Clock, EndpointProbe, LlmClient, LlmEndpoint, LlmError, OpenAiCompatibleClient, RetryPolicy,
    StructuredOutputMode, SystemClock,
};

/// What the Codex client fails with: a model-service failure or a sign-in
/// problem. The [`LanguageModel`] impl boxes the inner error, so a caller
/// downcasts to [`LlmError`] or [`CodexCredentialError`] directly.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CodexError {
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error(transparent)]
    Credential(#[from] CodexCredentialError),
}

impl CodexError {
    /// The inner error boxed, so a caller downcasts to [`LlmError`] or
    /// [`CodexCredentialError`] without knowing about this wrapper.
    #[must_use]
    pub fn boxed(self) -> BoxError {
        match self {
            CodexError::Llm(error) => Box::new(error),
            CodexError::Credential(error) => Box::new(error),
        }
    }
}

/// The Codex [`LlmClient`]: the Responses API on OpenAI's Codex backend with
/// the ChatGPT sign-in from [`CodexCredentialStore`]. Same shape as
/// [`OpenAiCompatibleClient`] (per-attempt timeout on the injected clock,
/// exponential retries, structured output mode fallback remembered per
/// client, every secret redacted from every error), with three differences
/// the backend forces: the answer arrives as an event stream and is
/// buffered whole, a 401 refreshes the sign-in once before it counts, and
/// no output ceiling or temperature is sent because the backend rejects
/// them. Identifies itself as Steno (`User-Agent`, `originator`).
pub struct CodexResponsesClient {
    endpoint: LlmEndpoint,
    credentials: Arc<CodexCredentialStore>,
    http: reqwest::Client,
    retry: RetryPolicy,
    clock: Arc<dyn Clock>,
    observer: Option<Observer>,
    mode: Mutex<StructuredOutputMode>,
    /// One per client, so the backend can group a meeting's requests.
    session_id: String,
}

impl CodexResponsesClient {
    /// The `client_version` the model list is filtered by: the server hides
    /// models newer than the Codex version named, so a high sentinel shows
    /// them all. A capability filter, not who we are; that is in the
    /// headers.
    pub const MODEL_LIST_CLIENT_VERSION: &'static str = "99.0.0";
    pub const ORIGINATOR: &'static str = "steno";

    pub const PLAN_LIMIT_KINDS: [&'static str; 2] = ["usage_limit_reached", "usage_not_included"];
    /// Failures the same request will hit again.
    pub const FINAL_STREAM_CODES: [&'static str; 4] = [
        "context_length_exceeded",
        "invalid_prompt",
        "insufficient_quota",
        "invalid_request_error",
    ];

    #[must_use]
    pub fn new(endpoint: LlmEndpoint, credentials: Arc<CodexCredentialStore>) -> Self {
        let mode = endpoint.structured_output_mode;
        CodexResponsesClient {
            endpoint,
            credentials,
            http: transport::default_http_client(),
            retry: RetryPolicy::default(),
            clock: Arc::new(SystemClock::default()),
            observer: None,
            mode: Mutex::new(mode),
            session_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    #[must_use]
    pub fn with_http(mut self, http: reqwest::Client) -> Self {
        self.http = http;
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

    fn mode(&self) -> StructuredOutputMode {
        *self.mode.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Moves the remembered mode from `from` to `to` only while it still is
    /// `from`: a late 400 from a concurrent request that started under an
    /// older mode must neither bounce the mode back up nor announce the
    /// same downgrade twice. Returns whether this call moved it.
    fn downgrade(&self, from: StructuredOutputMode, to: StructuredOutputMode) -> bool {
        let mut mode = self.mode.lock().unwrap_or_else(PoisonError::into_inner);
        if *mode != from {
            return false;
        }
        *mode = to;
        true
    }

    /// One completion with this crate's own error types.
    pub async fn complete_llm(&self, request: &LlmRequest) -> Result<LlmResponse, CodexError> {
        // Shared by every attempt of this completion; atomic only so the
        // attempt futures borrow nothing from the loop's closure.
        let refreshed_after_unauthorized = AtomicBool::new(false);
        let refreshed = &refreshed_after_unauthorized;
        transport::run_attempts(
            &self.retry,
            self.clock.as_ref(),
            self.observer.as_ref(),
            move |attempt| async move {
                match self.attempt(request, attempt, refreshed).await {
                    Ok(outcome) => Ok(outcome),
                    // The token endpoint hiccuped: a transport failure like
                    // any other, worth the same backoff. The other credential
                    // errors are final.
                    Err(CodexCredentialError::RefreshFailed(detail)) => {
                        Ok(Attempt::Failed(LlmError::Transport(detail)))
                    }
                    Err(error) => Err(CodexError::Credential(error)),
                }
            },
        )
        .await
    }

    async fn attempt(
        &self,
        request: &LlmRequest,
        attempt: u32,
        refreshed_after_unauthorized: &AtomicBool,
    ) -> Result<Attempt, CodexCredentialError> {
        let credentials = self.credentials.current().await?;
        let mode = self.mode();
        let wire = match self.make_request(request, mode, &credentials) {
            Ok(wire) => wire,
            Err(failure) => return Ok(Attempt::Failed(failure)),
        };
        notify(
            self.observer.as_ref(),
            LlmClientEvent::Request {
                attempt,
                purpose: request.purpose.clone(),
                mode,
            },
        );
        let secrets = credentials.secrets();
        let reply = match self.perform(wire, &secrets).await {
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
            return Ok(match Self::parse(&reply, &secrets) {
                Ok(response) => Attempt::Done(response),
                Err(failure) => Attempt::Failed(failure),
            });
        }
        if reply.status == 401 && !refreshed_after_unauthorized.load(Ordering::Relaxed) {
            // The file may hold a token the CLI already rotated (no
            // network), else one refresh; then a 401 is the answer.
            refreshed_after_unauthorized.store(true, Ordering::Relaxed);
            self.credentials
                .refreshed_if_still_using(&credentials.access_token)
                .await?;
            return Ok(Attempt::Resend);
        }
        if reply.status == 400
            && request.response_format.kind() != LlmResponseFormatKind::Text
            && Self::complains_about_text_format(&reply.body_text())
            && let Some(next) = mode.downgraded()
        {
            if self.downgrade(mode, next) {
                notify(self.observer.as_ref(), LlmClientEvent::ModeDowngraded(next));
            }
            return Ok(Attempt::Resend);
        }
        Ok(Attempt::Failed(Self::classify(&reply, &secrets)))
    }

    /// Every model the backend offers this account, in the server's order;
    /// [`CodexModel::is_listed`] marks the ones a picker should show. Fails
    /// with the credential error when there is no sign-in.
    pub async fn list_models(&self) -> Result<Vec<CodexModel>, CodexError> {
        let credentials = self.credentials.current().await?;
        let secrets = credentials.secrets();
        let request = self.models_request(&credentials)?;
        let reply = self.perform(request, &secrets).await?;
        if !reply.is_success() {
            return Err(Self::classify(&reply, &secrets).into());
        }
        let list: CodexModelList = wire::decode(&reply.body).map_err(|_| {
            LlmError::Transport(format!(
                "undecodable model list: {}",
                transport::redact(&reply.body_text(), &secrets)
            ))
        })?;
        Ok(list.models)
    }

    /// The sign-in's account line (the file, refreshed first when due), the
    /// model list (tolerated when it fails) and one tiny structured
    /// completion. Any failure of the completion is returned.
    pub async fn probe_llm(&self) -> Result<EndpointProbe, CodexError> {
        let credentials = self.credentials.current().await?;
        let start = self.clock.now();
        let model_listed = match self.list_models().await {
            Ok(models) => Some(models.iter().any(|model| model.slug == self.endpoint.model)),
            Err(_) => None,
        };
        self.complete_llm(&OpenAiCompatibleClient::probe_request())
            .await?;
        Ok(EndpointProbe {
            model_listed,
            resolved_mode: self.mode(),
            round_trip: self.clock.now().saturating_sub(start),
            account_line: Some(credentials.account_line()),
        })
    }

    // Requests

    /// `low` for the many small cleanup chunks and the probe, `medium` for
    /// the summary passes.
    #[must_use]
    pub fn reasoning_effort(purpose: &str) -> &'static str {
        if purpose.starts_with("cleanup") || purpose == "probe" {
            "low"
        } else {
            "medium"
        }
    }

    /// The wire `text.format` for a request under `mode`; `None` sends
    /// none.
    #[must_use]
    pub fn text_format(
        format: &LlmResponseFormat,
        mode: StructuredOutputMode,
    ) -> Option<ResponsesFormat> {
        Some(match mode.wire_format(format)? {
            WireFormat::JsonObject => ResponsesFormat::json_object(),
            WireFormat::JsonSchema {
                name,
                schema,
                strict,
            } => ResponsesFormat::json_schema(name, schema.clone(), strict),
        })
    }

    #[must_use]
    pub fn request_body(
        request: &LlmRequest,
        model: &str,
        mode: StructuredOutputMode,
    ) -> ResponsesRequest {
        let system: Vec<&str> = request
            .messages
            .iter()
            .filter(|message| message.role == LlmRole::System)
            .map(|message| message.content.as_str())
            .collect();
        let input = request
            .messages
            .iter()
            .filter(|message| message.role != LlmRole::System)
            .map(Into::into)
            .collect();
        ResponsesRequest {
            instructions: (!system.is_empty()).then(|| system.join("\n\n")),
            reasoning: Some(ResponsesReasoning {
                effort: Self::reasoning_effort(&request.purpose).to_owned(),
            }),
            text: Self::text_format(&request.response_format, mode)
                .map(|format| ResponsesText { format }),
            ..ResponsesRequest::new(model, input)
        }
    }

    fn make_request(
        &self,
        request: &LlmRequest,
        mode: StructuredOutputMode,
        credentials: &CodexCredentials,
    ) -> Result<reqwest::Request, LlmError> {
        let body = Self::request_body(request, &self.endpoint.model, mode);
        let bytes = wire::encode(&body).map_err(|error| LlmError::Transport(error.to_string()))?;
        self.http
            .post(self.endpoint.responses_url())
            .headers(self.headers(&request.purpose, credentials))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .body(bytes)
            .build()
            .map_err(|error| LlmError::Transport(error.to_string()))
    }

    fn models_request(&self, credentials: &CodexCredentials) -> Result<reqwest::Request, LlmError> {
        let mut url = self.endpoint.models_url();
        url.query_pairs_mut()
            .append_pair("client_version", Self::MODEL_LIST_CLIENT_VERSION);
        self.http
            .get(url)
            .headers(self.headers("models", credentials))
            .header(reqwest::header::ACCEPT, "application/json")
            .build()
            .map_err(|error| LlmError::Transport(error.to_string()))
    }

    fn headers(&self, purpose: &str, credentials: &CodexCredentials) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            crate::USER_AGENT.parse().expect("ascii"),
        );
        headers.insert("originator", Self::ORIGINATOR.parse().expect("ascii"));
        if let Ok(value) = purpose.parse() {
            headers.insert("X-Steno-Purpose", value);
        }
        for (name, text) in [
            (
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", credentials.access_token),
            ),
            (
                reqwest::header::HeaderName::from_static("chatgpt-account-id"),
                credentials.account_id.clone(),
            ),
        ] {
            if let Ok(mut value) = reqwest::header::HeaderValue::from_str(&text) {
                // Sensitive: the `Debug` form of the headers and of any
                // request built from them prints `Sensitive`, not the token.
                value.set_sensitive(true);
                headers.insert(name, value);
            }
        }
        if let Ok(value) = self.session_id.parse() {
            headers.insert("session-id", value);
        }
        headers
    }

    /// One attempt raced against `endpoint.request_timeout` on the clock.
    async fn perform(
        &self,
        request: reqwest::Request,
        secrets: &[String],
    ) -> Result<HttpReply, LlmError> {
        transport::perform(
            &self.http,
            request,
            self.clock.as_ref(),
            self.endpoint.request_timeout,
            secrets,
        )
        .await
    }

    // Replies

    /// A 2xx body as the response: the buffered event stream, or a backend
    /// that answered the whole response object at once.
    pub fn parse(reply: &HttpReply, secrets: &[String]) -> Result<LlmResponse, LlmError> {
        if wire::looks_like_event_stream(
            reply.headers.get("content-type").map(String::as_str),
            &reply.body,
        ) {
            return Self::parse_stream(&reply.body, secrets);
        }
        let response: ResponsesResponse = wire::decode(&reply.body).map_err(|_| {
            LlmError::Transport(format!(
                "undecodable response body: {}",
                transport::redact(&reply.body_text(), secrets)
            ))
        })?;
        let items = response.output.clone().unwrap_or_default();
        Self::result(&response, &items, secrets)
    }

    /// The buffered event stream to one response: message items from
    /// `response.output_item.done`, the status and usage from
    /// `response.completed` or `response.incomplete`. `response.failed` and
    /// `error` fail: a plan limit or another final code as a non-retryable
    /// `Http`, anything else (overload, unknown) as a retryable `Transport`.
    /// A stream that ends without a terminal event is a transport failure
    /// (the connection was cut).
    pub fn parse_stream(body: &[u8], secrets: &[String]) -> Result<LlmResponse, LlmError> {
        let mut items: Vec<ResponsesOutputItem> = Vec::new();
        let mut terminal: Option<ResponsesResponse> = None;
        for event in wire::parse_event_stream(&String::from_utf8_lossy(body)) {
            let Ok(decoded) = wire::decode::<ResponsesStreamEvent>(event.data.as_bytes()) else {
                continue;
            };
            match event.event.as_deref().unwrap_or(decoded.kind.as_str()) {
                "response.output_item.done" => {
                    if let Some(item) = decoded.item {
                        items.push(item);
                    }
                }
                "response.completed" | "response.incomplete" => terminal = decoded.response,
                "response.failed" => {
                    let error = decoded.response.and_then(|response| response.error);
                    return Err(Self::stream_failure(
                        error.as_ref().and_then(|e| e.code.as_deref()),
                        error
                            .as_ref()
                            .and_then(|e| e.message.as_deref())
                            .unwrap_or("the response failed"),
                        secrets,
                    ));
                }
                "error" => {
                    return Err(Self::stream_failure(
                        decoded.code.as_deref(),
                        decoded
                            .message
                            .as_deref()
                            .or(decoded.code.as_deref())
                            .unwrap_or("stream error"),
                        secrets,
                    ));
                }
                _ => {}
            }
        }
        let terminal = terminal.ok_or_else(|| {
            LlmError::Transport("stream closed before response.completed".to_owned())
        })?;
        if items.is_empty() {
            items = terminal.output.clone().unwrap_or_default();
        }
        Self::result(&terminal, &items, secrets)
    }

    fn result(
        response: &ResponsesResponse,
        items: &[ResponsesOutputItem],
        secrets: &[String],
    ) -> Result<LlmResponse, LlmError> {
        let messages: Vec<&ResponsesOutputItem> =
            items.iter().filter(|item| item.kind == "message").collect();
        if let Some(refusal) = messages.iter().find_map(|item| item.refusal())
            && !refusal.is_empty()
        {
            return Err(LlmError::Refused(transport::redact(refusal, secrets)));
        }
        let reason = response
            .incomplete_details
            .as_ref()
            .and_then(|details| details.reason.as_deref());
        let finish_reason = match (response.status.as_deref(), reason) {
            (Some("completed") | None, _) => LlmFinishReason::Stop,
            (Some("incomplete"), Some("max_output_tokens")) => LlmFinishReason::Length,
            (Some("incomplete"), Some("content_filter")) => LlmFinishReason::ContentFilter,
            _ => LlmFinishReason::Other,
        };
        let usage = response.usage.clone().unwrap_or_default();
        Ok(LlmResponse {
            text: messages.iter().map(|item| item.text()).collect(),
            finish_reason,
            usage: Some(LlmUsage {
                prompt_tokens: usage.input_tokens.unwrap_or(0),
                completion_tokens: usage.output_tokens.unwrap_or(0),
                requests: 1,
            }),
            model: response.model.clone(),
        })
    }

    /// A 429 is a rate limit unless the body names the plan's usage limit,
    /// which no backoff cures within a meeting; that and every other status
    /// are `Http`, not retried unless 408 or 5xx.
    #[must_use]
    pub fn classify(reply: &HttpReply, secrets: &[String]) -> LlmError {
        let envelope = wire::decode::<CodexErrorEnvelope>(&reply.body).ok();
        let fallback: String = reply.body_text().chars().take(500).collect();
        let message = transport::redact(
            envelope
                .as_ref()
                .and_then(CodexErrorEnvelope::message)
                .unwrap_or(fallback.as_str()),
            secrets,
        );
        if let Some(kind) = envelope.as_ref().and_then(CodexErrorEnvelope::kind)
            && Self::PLAN_LIMIT_KINDS.contains(&kind.as_str())
        {
            return LlmError::Http {
                status: reply.status,
                body: format!("ChatGPT plan limit reached: {message}"),
            };
        }
        if reply.status == 429 {
            return transport::rate_limited(reply);
        }
        LlmError::Http {
            status: reply.status,
            body: message,
        }
    }

    /// A `response.failed` or `error` event as an [`LlmError`]: final codes
    /// (the plan's limit, a prompt the backend will never take) are `Http`
    /// so no backoff runs; the rest are `Transport` and retried.
    #[must_use]
    pub fn stream_failure(code: Option<&str>, message: &str, secrets: &[String]) -> LlmError {
        let code = code.map(str::to_lowercase);
        let message = transport::redact(message, secrets);
        if let Some(code) = code.as_deref()
            && Self::PLAN_LIMIT_KINDS.contains(&code)
        {
            return LlmError::Http {
                status: 400,
                body: format!("ChatGPT plan limit reached: {message}"),
            };
        }
        if let Some(code) = code.as_deref()
            && Self::FINAL_STREAM_CODES.contains(&code)
        {
            return LlmError::Http {
                status: 400,
                body: format!("{code}: {message}"),
            };
        }
        LlmError::Transport(message)
    }

    /// A 400 that names the structured output request (the Responses API
    /// spells it `text.format`): the cue to fall back one mode.
    #[must_use]
    pub fn complains_about_text_format(body: &str) -> bool {
        body.to_lowercase().contains("text.format")
            || OpenAiCompatibleClient::complains_about_response_format(body)
    }
}

#[async_trait]
impl LanguageModel for CodexResponsesClient {
    async fn complete(&self, request: &LlmRequest) -> BoundaryResult<LlmResponse> {
        self.complete_llm(request).await.map_err(CodexError::boxed)
    }
}

#[async_trait]
impl LlmClient for CodexResponsesClient {
    fn endpoint(&self) -> &LlmEndpoint {
        &self.endpoint
    }

    fn resolved_mode(&self) -> StructuredOutputMode {
        self.mode()
    }

    async fn probe(&self) -> BoundaryResult<EndpointProbe> {
        self.probe_llm().await.map_err(CodexError::boxed)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn the_token_and_account_headers_are_sensitive() {
        let client = CodexResponsesClient::new(
            LlmEndpoint::codex("m", 32_000),
            Arc::new(CodexCredentialStore::new("/nonexistent")),
        );
        let credentials = CodexCredentials {
            access_token: "at-unit-secret".to_owned(),
            refresh_token: "rt-unit-secret".to_owned(),
            account_id: "acct-unit-secret".to_owned(),
            email: None,
            plan_type: None,
            expires_at: None,
            last_refresh: None,
        };
        let headers = client.headers("test", &credentials);
        assert_eq!(
            headers[reqwest::header::AUTHORIZATION].to_str().unwrap(),
            "Bearer at-unit-secret"
        );
        assert_eq!(
            headers["chatgpt-account-id"].to_str().unwrap(),
            "acct-unit-secret"
        );
        let debug = format!("{headers:?}");
        for secret in credentials.secrets() {
            assert!(!debug.contains(&secret), "{debug}");
        }
    }
}
