//! The OpenAI chat completions wire format: `POST {base}/chat/completions`
//! and `GET {base}/models`.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use steno_core::LlmMessage;

/// The request body of `POST /chat/completions`. `max_completion_tokens` is
/// sent instead of `max_tokens` once a server rejected the latter by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ChatResponseFormat>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl From<&LlmMessage> for ChatMessage {
    fn from(message: &LlmMessage) -> Self {
        ChatMessage {
            role: message.role.as_str().to_owned(),
            content: message.content.clone(),
        }
    }
}

/// `response_format`: `{"type": "json_object"}` or
/// `{"type": "json_schema", "json_schema": {"name", "schema", "strict"}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponseFormat {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<ChatResponseSchema>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponseSchema {
    pub name: String,
    pub schema: Value,
    pub strict: bool,
}

impl ChatResponseFormat {
    #[must_use]
    pub fn json_object() -> Self {
        ChatResponseFormat {
            kind: "json_object".to_owned(),
            json_schema: None,
        }
    }

    #[must_use]
    pub fn json_schema(name: &str, schema: Value, strict: bool) -> Self {
        ChatResponseFormat {
            kind: "json_schema".to_owned(),
            json_schema: Some(ChatResponseSchema {
                name: name.to_owned(),
                schema,
                strict,
            }),
        }
    }
}

/// The response body of `POST /chat/completions`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub choices: Vec<ChatChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ChatUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatChoice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<i64>,
    pub message: ChatChoiceMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

/// `content` is a string or an array of text parts, decoded to their
/// concatenation. `refusal` is OpenAI's structured-output refusal.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChatChoiceMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

impl ChatChoiceMessage {
    #[must_use]
    pub fn assistant(content: Option<&str>, refusal: Option<&str>) -> Self {
        ChatChoiceMessage {
            role: Some("assistant".to_owned()),
            content: content.map(str::to_owned),
            refusal: refusal.map(str::to_owned),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ContentField {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Deserialize)]
struct ContentPart {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct RawChoiceMessage {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<ContentField>,
    #[serde(default)]
    refusal: Option<String>,
}

impl<'de> Deserialize<'de> for ChatChoiceMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawChoiceMessage::deserialize(deserializer)?;
        let content = match raw.content {
            Some(ContentField::Text(text)) => Some(text),
            Some(ContentField::Parts(parts)) => {
                Some(parts.into_iter().filter_map(|part| part.text).collect())
            }
            None => None,
        };
        Ok(ChatChoiceMessage {
            role: raw.role,
            content,
            refusal: raw.refusal,
        })
    }
}

/// Every count is optional, so a partial `usage` never fails a good answer.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChatUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
}

/// The error envelope. `code` is a string or an integer, so it is any JSON
/// value; `param` names the rejected request field on a 400.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatErrorEnvelope {
    pub error: ChatErrorDetail,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ChatErrorDetail {
    pub message: String,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Value>,
}

impl ChatErrorEnvelope {
    #[must_use]
    pub fn message(message: &str) -> Self {
        ChatErrorEnvelope {
            error: ChatErrorDetail {
                message: message.to_owned(),
                ..ChatErrorDetail::default()
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelList {
    pub data: Vec<ModelListEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelListEntry {
    pub id: String,
}
