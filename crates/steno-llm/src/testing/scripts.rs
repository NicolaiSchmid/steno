//! Ready-made [`StubResponse`]s for the situations the client must survive.

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

/// Answers a request the queue did not cover; `None` gives a 404.
pub type Responder = Arc<dyn Fn(&RecordedRequest) -> Option<StubResponse> + Send + Sync>;

/// The canned responses for the stub server: completions, rate limits,
/// server errors, rejections, refusals, model lists, and the Codex
/// backend's streams and token endpoint answers.
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

    /// [`Self::completion`] with `stop`, the default usage and stub model.
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

    /// A 429 whose `Retry-After` header is `retry_after` verbatim.
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
    /// produce.
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

    /// The 400 Groq and OpenRouter send when a model lacks structured output.
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

    /// OpenAI's 400 for a request field the model does not take, named in
    /// `error.param`.
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

    /// Plays an OpenAI reasoning model: rejects `max_tokens` and any
    /// `temperature` by name, else returns `completion`.
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

    /// Plays a perfect cleanup model, passing each segment through
    /// `transform(index, text)`; `None` drops it from the answer.
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

    /// Answers `GET /models` with `models`, rejects `response_format` kinds
    /// in `rejecting` with a 400, else returns `completion`.
    #[must_use]
    pub fn format_rejecting_server(
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

    /// A Codex `text/event-stream` body: created, one message item, then
    /// `response.completed` (or `response.incomplete`).
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

    /// [`Self::responses_stream`] with the default usage and stub model.
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

    /// A stream ending in a top-level `error` event with `message`.
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

    /// A `text/event-stream` body written verbatim.
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
    pub fn codex_rejects_parameter(&self, name: &str) -> StubResponse {
        StubResponse::new(
            400,
            format!("{{\"detail\":\"Unsupported parameter: {name}\"}}").into_bytes(),
        )
        .with_header("Content-Type", "application/json")
    }

    /// The plan's usage limit, which the client must not retry.
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

    /// A refreshed token triple from the token endpoint; `None` for
    /// `refresh` plays an endpoint that did not rotate it.
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

/// A `response.*` event named `kind`.
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
