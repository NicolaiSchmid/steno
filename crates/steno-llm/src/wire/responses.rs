//! The OpenAI Responses API as the Codex backend speaks it:
//! `POST {codex}/responses`, always streamed, and `GET {codex}/models`.
//! Only the fields Steno sends or reads; the backend rejects `temperature`
//! and `max_output_tokens` by name, so neither has a place here.
//! Swift: `Sources/StenoLLM/Wire/Responses.swift`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use steno_core::{LlmMessage, LlmRole};

/// The request body of `POST /responses`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponsesRequest {
    pub model: String,
    /// The system prompt; the Responses API takes it apart from the input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    pub input: Vec<ResponsesInputItem>,
    pub tool_choice: String,
    pub parallel_tool_calls: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ResponsesReasoning>,
    pub store: bool,
    pub stream: bool,
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<ResponsesText>,
}

impl ResponsesRequest {
    /// The fixed part of every request: no tools, nothing stored, streamed.
    #[must_use]
    pub fn new(model: &str, input: Vec<ResponsesInputItem>) -> Self {
        ResponsesRequest {
            model: model.to_owned(),
            instructions: None,
            input,
            tool_choice: "auto".to_owned(),
            parallel_tool_calls: false,
            reasoning: None,
            store: false,
            stream: true,
            include: Vec::new(),
            text: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponsesReasoning {
    pub effort: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponsesText {
    pub format: ResponsesFormat,
}

/// `{"type": "json_object"}` or
/// `{"type": "json_schema", "name", "schema", "strict"}` (flat, unlike chat
/// completions' nested `json_schema`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponsesFormat {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

impl ResponsesFormat {
    #[must_use]
    pub fn json_object() -> Self {
        ResponsesFormat {
            kind: "json_object".to_owned(),
            name: None,
            schema: None,
            strict: None,
        }
    }

    #[must_use]
    pub fn json_schema(name: &str, schema: Value, strict: bool) -> Self {
        ResponsesFormat {
            kind: "json_schema".to_owned(),
            name: Some(name.to_owned()),
            schema: Some(schema),
            strict: Some(strict),
        }
    }
}

/// One `message` input item: a role and its text parts. User text goes as
/// `input_text`; an earlier assistant answer (the repair round) as
/// `output_text`, which is how the API names assistant-authored content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponsesInputItem {
    #[serde(rename = "type")]
    pub kind: String,
    pub role: String,
    pub content: Vec<ResponsesInputPart>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponsesInputPart {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}

impl From<&LlmMessage> for ResponsesInputItem {
    fn from(message: &LlmMessage) -> Self {
        let kind = if message.role == LlmRole::Assistant {
            "output_text"
        } else {
            "input_text"
        };
        ResponsesInputItem {
            kind: "message".to_owned(),
            role: message.role.as_str().to_owned(),
            content: vec![ResponsesInputPart {
                kind: kind.to_owned(),
                text: message.content.clone(),
            }],
        }
    }
}

/// A `response` object as the stream's `response.*` events carry it. Only
/// the fields Steno reads; `output` is used when no item event was seen.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ResponsesResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Vec<ResponsesOutputItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ResponsesUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_details: Option<ResponsesIncompleteDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponsesError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResponsesIncompleteDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResponsesError {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `input_tokens` and `output_tokens`; both optional so a missing usage
/// never fails a good answer.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResponsesUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
}

/// One output item. Steno reads `message` items' `output_text` parts and
/// treats a `refusal` part as the model declining; reasoning and tool items
/// are skipped.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResponsesOutputItem {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ResponsesOutputPart>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResponsesOutputPart {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

impl ResponsesOutputItem {
    /// A finished assistant message with one `output_text` part.
    #[must_use]
    pub fn message(id: Option<&str>, text: &str) -> Self {
        ResponsesOutputItem {
            kind: "message".to_owned(),
            id: id.map(str::to_owned),
            role: Some("assistant".to_owned()),
            content: Some(vec![ResponsesOutputPart {
                kind: "output_text".to_owned(),
                text: Some(text.to_owned()),
                refusal: None,
            }]),
        }
    }

    /// The `output_text` parts joined.
    #[must_use]
    pub fn text(&self) -> String {
        self.content
            .iter()
            .flatten()
            .filter(|part| part.kind == "output_text")
            .filter_map(|part| part.text.as_deref())
            .collect()
    }

    /// The first `refusal` part's text.
    #[must_use]
    pub fn refusal(&self) -> Option<&str> {
        self.content
            .iter()
            .flatten()
            .find(|part| part.kind == "refusal")
            .and_then(|part| part.refusal.as_deref())
    }
}

/// One event of the stream, decoded from its `data:` JSON. `type` matches
/// the SSE `event:` name (`response.completed`, `response.output_item.done`,
/// `error`, …).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ResponsesStreamEvent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<ResponsesResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ResponsesOutputItem>,
    /// The top-level `error` event's payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// The Codex backend's error bodies come in two shapes:
/// `{"detail": "Unsupported parameter: …"}` and OpenAI's
/// `{"error": {"message", "type", "code"}}`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct CodexErrorEnvelope {
    #[serde(default)]
    pub detail: Option<Value>,
    #[serde(default)]
    pub error: Option<CodexErrorDetail>,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct CodexErrorDetail {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub code: Option<Value>,
}

impl CodexErrorEnvelope {
    /// The most specific human sentence in the body.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        if let Some(text) = self.error.as_ref().and_then(|e| e.message.as_deref())
            && !text.is_empty()
        {
            return Some(text);
        }
        match &self.detail {
            Some(Value::String(text)) => Some(text),
            Some(Value::Object(object)) => object.get("message").and_then(Value::as_str),
            _ => None,
        }
    }

    /// `error.type` or `error.code`, lowercased, for the plan-limit check.
    #[must_use]
    pub fn kind(&self) -> Option<String> {
        let error = self.error.as_ref()?;
        if let Some(kind) = error.kind.as_deref().filter(|k| !k.is_empty()) {
            return Some(kind.to_lowercase());
        }
        match &error.code {
            Some(Value::String(code)) if !code.is_empty() => Some(code.to_lowercase()),
            _ => None,
        }
    }
}

/// One entry of `GET {codex}/models`. `visibility` is "list" for models the
/// picker should show; the rest are hidden or retired.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CodexModel {
    pub slug: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
}

impl CodexModel {
    #[must_use]
    pub fn new(slug: &str, display_name: &str) -> Self {
        CodexModel {
            slug: slug.to_owned(),
            display_name: display_name.to_owned(),
            visibility: Some("list".to_owned()),
            context_window: None,
        }
    }

    #[must_use]
    pub fn is_listed(&self) -> bool {
        self.visibility.as_deref().unwrap_or("list") == "list"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexModelList {
    pub models: Vec<CodexModel>,
}
