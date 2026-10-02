//! Ready-made [`StubResponse`]s for the situations the client must survive.
//! Swift: `Sources/StenoLLM/Testing/Scripts.swift`.

use std::sync::Arc;

use serde::Serialize;
use steno_core::LlmUsage;

use super::{RecordedRequest, StubResponse};
use crate::cleanup::{CleanupDraft, CleanupDraftSegment};
use crate::wire::{
    self, ChatChoice, ChatChoiceMessage, ChatCompletionResponse, ChatErrorDetail,
    ChatErrorEnvelope, ChatUsage, CodexModel, CodexModelList, ModelList, ModelListEntry,
    ResponsesError, ResponsesIncompleteDetails, ResponsesOutputItem, ResponsesOutputPart,
    ResponsesResponse, ResponsesStreamEvent, ResponsesUsage,
};

/// Answers a request the queue did not cover; `None` falls through to a
/// 404.
pub type Responder = Arc<dyn Fn(&RecordedRequest) -> Option<StubResponse> + Send + Sync>;

/// The canned responses: a plain completion, a rate limit with
/// `Retry-After`, a server error, a 400 rejecting `response_format`, fenced
/// or invalid JSON, a truncated answer, a refusal, a model list, and the
/// Codex backend's streams and token endpoint answers.
/// [`StubResponse::hang`] and [`StubResponse::drop_connection`] cover a
/// connection that never answers or closes early.
#[allow(non_upper_case_globals)]
pub const scripts: Scripts = Scripts;

pub struct Scripts;

/// The usage most scripts report.
#[must_use]
pub fn default_usage() -> LlmUsage {
    LlmUsage {
        prompt_tokens: 10,
        completion_tokens: 5,
        requests: 1,
    }
}

impl Scripts {
    /// A successful completion whose message content is `text`.
    #[must_use]
    pub fn completion(
        &self,
        text: &str,
        finish_reason: Option<&str>,
        usage: Option<LlmUsage>,
        model: &str,
    ) -> StubResponse {
        StubResponse::json(
            &ChatCompletionResponse {
                id: Some("chatcmpl-stub".to_owned()),
                model: Some(model.to_owned()),
                choices: vec![ChatChoice {
                    index: Some(0),
                    message: ChatChoiceMessage::assistant(Some(text), None),
                    finish_reason: finish_reason.map(str::to_owned),
                }],
                usage: usage.map(|usage| ChatUsage {
                    prompt_tokens: Some(usage.prompt_tokens),
                    completion_tokens: Some(usage.completion_tokens),
                    total_tokens: Some(usage.prompt_tokens + usage.completion_tokens),
                }),
            },
            200,
        )
    }

    /// [`Self::completion`] with `stop`, the default usage and the stub
    /// model.
    #[must_use]
    pub fn text(&self, text: &str) -> StubResponse {
        self.completion(text, Some("stop"), Some(default_usage()), "stub-model")
    }

    /// A completion whose content is `value` encoded as JSON.
    #[must_use]
    pub fn json<T: Serialize>(&self, value: &T, usage: Option<LlmUsage>) -> StubResponse {
        self.completion(&encoded(value), Some("stop"), usage, "stub-model")
    }

    /// `value` as JSON inside a Markdown fence with a prose prefix.
    #[must_use]
    pub fn fenced<T: Serialize>(&self, value: &T) -> StubResponse {
        self.text(&format!(
            "Here is the JSON you asked for:\n```json\n{}\n```\n",
            encoded(value)
        ))
    }

    /// A completion cut off by the server's token limit.
    #[must_use]
    pub fn truncated(&self, partial_text: &str) -> StubResponse {
        self.completion(
            partial_text,
            Some("length"),
            Some(default_usage()),
            "stub-model",
        )
    }

    /// OpenAI's structured-output refusal: no content, a `refusal` string.
    #[must_use]
    pub fn refusal(&self, reason: &str) -> StubResponse {
        StubResponse::json(
            &ChatCompletionResponse {
                model: Some("stub-model".to_owned()),
                choices: vec![ChatChoice {
                    index: None,
                    message: ChatChoiceMessage::assistant(None, Some(reason)),
                    finish_reason: Some("stop".to_owned()),
                }],
                ..ChatCompletionResponse::default()
            },
            200,
        )
    }

    /// A 429 whose `Retry-After` header is `retry_after` verbatim, for the
    /// values a client must survive (`inf`, `1e300`, an HTTP date).
    #[must_use]
    pub fn rate_limited(&self, retry_after: Option<&str>) -> StubResponse {
        let response = StubResponse::json(
            &ChatErrorEnvelope {
                error: ChatErrorDetail {
                    message: "Rate limit reached".to_owned(),
                    kind: Some("rate_limit_error".to_owned()),
                    ..ChatErrorDetail::default()
                },
            },
            429,
        );
        match retry_after {
            Some(header) => response.with_header("Retry-After", header),
            None => response,
        }
    }

    /// A completion body written verbatim, for shapes the wire types cannot
    /// produce (a null or partial `usage`).
    #[must_use]
    pub fn raw_completion(&self, body: &str) -> StubResponse {
        StubResponse::new(200, body.as_bytes().to_vec())
            .with_header("Content-Type", "application/json")
    }

    #[must_use]
    pub fn server_error(&self, status: u16) -> StubResponse {
        StubResponse::json(
            &ChatErrorEnvelope::message("The server had an error"),
            status,
        )
    }

    #[must_use]
    pub fn unauthorized(&self) -> StubResponse {
        StubResponse::json(
            &ChatErrorEnvelope {
                error: ChatErrorDetail {
                    message: "Incorrect API key provided".to_owned(),
                    kind: Some("invalid_request_error".to_owned()),
                    ..ChatErrorDetail::default()
                },
            },
            401,
        )
    }

    /// The 400 Groq and OpenRouter send when a model lacks structured
    /// output.
    #[must_use]
    pub fn rejects_response_format(&self) -> StubResponse {
        StubResponse::json(
            &ChatErrorEnvelope {
                error: ChatErrorDetail {
                    message:
                        "'response_format' of type 'json_schema' is not supported with this model"
                            .to_owned(),
                    kind: Some("invalid_request_error".to_owned()),
                    param: None,
                    code: Some("response_format_unsupported".into()),
                },
            },
            400,
        )
    }

    #[must_use]
    pub fn bad_request(&self, message: &str) -> StubResponse {
        StubResponse::json(&ChatErrorEnvelope::message(message), 400)
    }

    /// OpenAI's 400 for a request field a model does not take, the field
    /// named in `error.param`: `max_tokens` and `temperature` on reasoning
    /// models.
    #[must_use]
    pub fn rejects_parameter(&self, param: &str) -> StubResponse {
        StubResponse::json(
            &ChatErrorEnvelope {
                error: ChatErrorDetail {
                    message: format!(
                        "Unsupported parameter: '{param}' is not supported with this model."
                    ),
                    kind: Some("invalid_request_error".to_owned()),
                    param: Some(param.to_owned()),
                    code: Some("unsupported_parameter".into()),
                },
            },
            400,
        )
    }

    /// A responder that plays an OpenAI reasoning model: it rejects
    /// `max_tokens` (wanting `max_completion_tokens`) and any
    /// `temperature`, each by name, and otherwise returns `completion`.
    #[must_use]
    pub fn reasoning_model(&self, completion: StubResponse) -> Responder {
        Arc::new(move |request: &RecordedRequest| {
            let Some(chat) = &request.chat else {
                return Some(completion.clone());
            };
            if chat.max_tokens.is_some() {
                return Some(scripts.rejects_parameter("max_tokens"));
            }
            if chat.temperature.is_some() {
                return Some(scripts.rejects_parameter("temperature"));
            }
            Some(completion.clone())
        })
    }

    #[must_use]
    pub fn models(&self, ids: &[&str]) -> StubResponse {
        StubResponse::json(
            &ModelList {
                data: ids
                    .iter()
                    .map(|id| ModelListEntry {
                        id: (*id).to_owned(),
                    })
                    .collect(),
            },
            200,
        )
    }

    /// A responder that plays a perfect cleanup model: it reads the
    /// numbered segments out of the request and answers with them, each
    /// passed through `transform(index, text)`. Segments the transform
    /// returns `None` for are dropped from the answer, which lets a test
    /// script a wrong count.
    #[must_use]
    pub fn cleanup_echo(
        &self,
        usage: LlmUsage,
        transform: impl Fn(i64, &str) -> Option<String> + Send + Sync + 'static,
    ) -> Responder {
        Arc::new(move |request: &RecordedRequest| {
            let user = request
                .chat
                .as_ref()?
                .messages
                .iter()
                .rev()
                .find(|message| message.role == "user")?;
            let draft = CleanupDraft {
                segments: parse_segments(&user.content)
                    .into_iter()
                    .filter_map(|(index, text)| {
                        transform(index, &text).map(|text| CleanupDraftSegment { index, text })
                    })
                    .collect(),
            };
            Some(scripts.json(&draft, Some(usage)))
        })
    }

    /// A responder that answers `GET /models` with `models`, rejects
    /// `response_format` kinds in `rejecting` with a 400, and otherwise
    /// returns `completion`. Models the servers that honour `json_object`
    /// but not `json_schema`.
    #[must_use]
    pub fn server(
        &self,
        models: &[&str],
        rejecting: &[&str],
        completion: StubResponse,
    ) -> Responder {
        let models: Vec<String> = models.iter().map(|m| (*m).to_owned()).collect();
        let rejecting: Vec<String> = rejecting.iter().map(|r| (*r).to_owned()).collect();
        Arc::new(move |request: &RecordedRequest| {
            if request.method == "GET" && request.path.ends_with("/models") {
                let ids: Vec<&str> = models.iter().map(String::as_str).collect();
                return Some(scripts.models(&ids));
            }
            if let Some(format) = request
                .chat
                .as_ref()
                .and_then(|chat| chat.response_format.as_ref())
                && rejecting.contains(&format.kind)
            {
                return Some(scripts.rejects_response_format());
            }
            Some(completion.clone())
        })
    }

    // Codex backend

    /// One `text/event-stream` body as the Codex backend streams a finished
    /// response: created, one message item done, then `response.completed`
    /// (or `response.incomplete` with `incomplete_reason`).
    #[must_use]
    pub fn responses_stream(
        &self,
        text: &str,
        status: &str,
        incomplete_reason: Option<&str>,
        usage: Option<LlmUsage>,
        model: &str,
    ) -> StubResponse {
        let item = ResponsesOutputItem::message(Some("msg_stub"), text);
        let response = ResponsesResponse {
            id: Some("resp_stub".to_owned()),
            status: Some(status.to_owned()),
            model: Some(model.to_owned()),
            output: Some(vec![item.clone()]),
            usage: usage.map(|usage| ResponsesUsage {
                input_tokens: Some(usage.prompt_tokens),
                output_tokens: Some(usage.completion_tokens),
            }),
            incomplete_details: incomplete_reason.map(|reason| ResponsesIncompleteDetails {
                reason: Some(reason.to_owned()),
            }),
            error: None,
        };
        let terminal = if status == "completed" {
            "response.completed"
        } else {
            "response.incomplete"
        };
        event_stream(&[
            event("response.created", Some(response.clone()), None),
            event("response.output_item.done", None, Some(item)),
            event(terminal, Some(response), None),
        ])
    }

    /// [`Self::responses_stream`] completed with the default usage and the
    /// stub model.
    #[must_use]
    pub fn stream(&self, text: &str) -> StubResponse {
        self.responses_stream(text, "completed", None, Some(default_usage()), "gpt-stub")
    }

    /// A stream that ends in `response.failed` with `message`.
    #[must_use]
    pub fn responses_failed(&self, message: &str) -> StubResponse {
        let response = ResponsesResponse {
            id: Some("resp_stub".to_owned()),
            status: Some("failed".to_owned()),
            error: Some(ResponsesError {
                code: Some("server_error".to_owned()),
                message: Some(message.to_owned()),
            }),
            ..ResponsesResponse::default()
        };
        event_stream(&[
            event("response.created", Some(response.clone()), None),
            event("response.failed", Some(response), None),
        ])
    }

    /// A stream with a `refusal` part instead of text.
    #[must_use]
    pub fn responses_refusal(&self, reason: &str) -> StubResponse {
        let item = ResponsesOutputItem {
            kind: "message".to_owned(),
            id: Some("msg_stub".to_owned()),
            role: Some("assistant".to_owned()),
            content: Some(vec![ResponsesOutputPart {
                kind: "refusal".to_owned(),
                text: None,
                refusal: Some(reason.to_owned()),
            }]),
        };
        let response = ResponsesResponse {
            id: Some("resp_stub".to_owned()),
            status: Some("completed".to_owned()),
            output: Some(vec![item.clone()]),
            ..ResponsesResponse::default()
        };
        event_stream(&[
            event("response.output_item.done", None, Some(item)),
            event("response.completed", Some(response), None),
        ])
    }

    /// A stream that ends in a top-level `error` event with `message`, the
    /// shape the backend uses for a failure that has no response object.
    #[must_use]
    pub fn responses_error_event(&self, message: &str, code: &str) -> StubResponse {
        event_stream(&[
            event(
                "response.created",
                Some(ResponsesResponse {
                    id: Some("resp_stub".to_owned()),
                    status: Some("in_progress".to_owned()),
                    ..ResponsesResponse::default()
                }),
                None,
            ),
            (
                "error",
                ResponsesStreamEvent {
                    kind: "error".to_owned(),
                    code: Some(code.to_owned()),
                    message: Some(message.to_owned()),
                    ..ResponsesStreamEvent::default()
                },
            ),
        ])
    }

    /// A `text/event-stream` body written verbatim, for shapes the typed
    /// events cannot produce (data-only events, `[DONE]`, unknown names).
    #[must_use]
    pub fn raw_event_stream(&self, body: &str) -> StubResponse {
        StubResponse::new(200, body.as_bytes().to_vec())
            .with_header("Content-Type", "text/event-stream")
    }

    /// A stream cut before its terminal event.
    #[must_use]
    pub fn responses_truncated_stream(&self) -> StubResponse {
        event_stream(&[event(
            "response.created",
            Some(ResponsesResponse {
                id: Some("resp_stub".to_owned()),
                status: Some("in_progress".to_owned()),
                ..ResponsesResponse::default()
            }),
            None,
        )])
    }

    /// The Codex backend's 400 for a request field it does not take.
    #[must_use]
    pub fn codex_unsupported_parameter(&self, name: &str) -> StubResponse {
        StubResponse::new(
            400,
            format!("{{\"detail\":\"Unsupported parameter: {name}\"}}").into_bytes(),
        )
        .with_header("Content-Type", "application/json")
    }

    /// The plan's usage limit: a 429 whose `error.type` names it, which the
    /// client must not retry.
    #[must_use]
    pub fn codex_usage_limit(&self) -> StubResponse {
        StubResponse::json(
            &ChatErrorEnvelope {
                error: ChatErrorDetail {
                    message: "You have hit your usage limit.".to_owned(),
                    kind: Some("usage_limit_reached".to_owned()),
                    param: None,
                    code: Some("usage_limit_reached".into()),
                },
            },
            429,
        )
        .with_header("Retry-After", "3600")
    }

    #[must_use]
    pub fn codex_models(&self, models: Vec<CodexModel>) -> StubResponse {
        StubResponse::json(&CodexModelList { models }, 200)
    }

    /// A refreshed token triple from the OAuth token endpoint. Every field
    /// is optional there; `None` for `refresh` plays an endpoint that did
    /// not rotate the refresh token.
    #[must_use]
    pub fn token_refresh(
        &self,
        access: &str,
        refresh: Option<&str>,
        id: Option<&str>,
    ) -> StubResponse {
        let mut body = serde_json::Map::new();
        body.insert("access_token".to_owned(), access.into());
        if let Some(refresh) = refresh {
            body.insert("refresh_token".to_owned(), refresh.into());
        }
        if let Some(id) = id {
            body.insert("id_token".to_owned(), id.into());
        }
        StubResponse::json(&body, 200)
    }

    /// The token endpoint refusing a refresh token for good.
    #[must_use]
    pub fn token_refresh_rejected(&self, code: &str, status: u16) -> StubResponse {
        StubResponse::json(
            &serde_json::json!({
                "error": "invalid_grant",
                "error_code": code,
                "error_description": format!("The refresh token is {code}."),
            }),
            status,
        )
    }
}

fn encoded<T: Serialize>(value: &T) -> String {
    wire::encode(value).map_or_else(
        |_| "{}".to_owned(),
        |bytes| String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// `[n] Label: text` lines after the "Segments to correct" marker.
#[must_use]
pub fn parse_segments(user_message: &str) -> Vec<(i64, String)> {
    let mut segments = Vec::new();
    let mut started = false;
    for line in user_message.split('\n') {
        if line.starts_with("Segments to correct") {
            started = true;
            continue;
        }
        if !started || !line.starts_with('[') {
            continue;
        }
        let Some(close) = line.find(']') else {
            continue;
        };
        let Ok(index) = line[1..close].parse::<i64>() else {
            continue;
        };
        let rest = &line[close + 1..];
        let Some(colon) = rest.find(':') else {
            continue;
        };
        segments.push((index, rest[colon + 1..].trim().to_owned()));
    }
    segments
}

/// A `response.*` event with `kind` as its name; `item` only for
/// `response.output_item.done`.
fn event(
    kind: &'static str,
    response: Option<ResponsesResponse>,
    item: Option<ResponsesOutputItem>,
) -> (&'static str, ResponsesStreamEvent) {
    (
        kind,
        ResponsesStreamEvent {
            kind: kind.to_owned(),
            response,
            item,
            ..ResponsesStreamEvent::default()
        },
    )
}

/// `event: name\ndata: json\n\n` per event, as `text/event-stream`.
#[must_use]
pub fn event_stream(events: &[(&str, ResponsesStreamEvent)]) -> StubResponse {
    use std::fmt::Write;
    let mut body = String::new();
    for (name, event) in events {
        let _ = write!(body, "event: {name}\ndata: {}\n\n", encoded(event));
    }
    StubResponse::new(200, body.into_bytes()).with_header("Content-Type", "text/event-stream")
}
