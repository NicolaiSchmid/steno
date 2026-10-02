//! The language model boundary's value types: token accounting, the
//! request and the response.
//! Swift: `Sources/StenoCore/Model/LLM.swift`.

use std::ops::Add;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::json::case_coding::{self, Case};
use crate::string_enum;

/// Token accounting summed over every LLM call of a meeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub requests: i64,
}

impl LlmUsage {
    pub const ZERO: LlmUsage = LlmUsage {
        prompt_tokens: 0,
        completion_tokens: 0,
        requests: 0,
    };
}

impl Add for LlmUsage {
    type Output = LlmUsage;

    fn add(self, rhs: LlmUsage) -> LlmUsage {
        LlmUsage {
            prompt_tokens: self.prompt_tokens + rhs.prompt_tokens,
            completion_tokens: self.completion_tokens + rhs.completion_tokens,
            requests: self.requests + rhs.requests,
        }
    }
}

string_enum! {
    pub enum LlmRole {
        System = "system",
        User = "user",
        Assistant = "assistant",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: LlmRole,
    pub content: String,
}

string_enum! {
    /// The case names of [`LlmResponseFormat`].
    pub enum LlmResponseFormatKind {
        Text = "text",
        JsonObject = "jsonObject",
        JsonSchema = "jsonSchema",
    }
}

/// How the model must shape its answer.
#[derive(Debug, Clone, PartialEq)]
pub enum LlmResponseFormat {
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        schema: Value,
        strict: bool,
    },
}

#[derive(Serialize, Deserialize)]
struct SchemaPayload {
    name: String,
    schema: Value,
    strict: bool,
}

impl LlmResponseFormat {
    #[must_use]
    pub fn kind(&self) -> LlmResponseFormatKind {
        match self {
            LlmResponseFormat::Text => LlmResponseFormatKind::Text,
            LlmResponseFormat::JsonObject => LlmResponseFormatKind::JsonObject,
            LlmResponseFormat::JsonSchema { .. } => LlmResponseFormatKind::JsonSchema,
        }
    }
}

impl Serialize for LlmResponseFormat {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let name = self.kind().as_str();
        match self {
            LlmResponseFormat::JsonSchema {
                name: schema_name,
                schema,
                strict,
            } => case_coding::serialize_payload(
                name,
                &SchemaPayload {
                    name: schema_name.clone(),
                    schema: schema.clone(),
                    strict: *strict,
                },
                serializer,
            ),
            _ => case_coding::serialize_bare(name, serializer),
        }
    }
}

impl<'de> Deserialize<'de> for LlmResponseFormat {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (name, payload) = Case::deserialize(deserializer)?.split()?;
        let kind: LlmResponseFormatKind = name.parse().map_err(serde::de::Error::custom)?;
        Ok(match kind {
            LlmResponseFormatKind::Text => LlmResponseFormat::Text,
            LlmResponseFormatKind::JsonObject => LlmResponseFormat::JsonObject,
            LlmResponseFormatKind::JsonSchema => {
                let SchemaPayload {
                    name,
                    schema,
                    strict,
                } = case_coding::payload(&name, payload)?;
                LlmResponseFormat::JsonSchema {
                    name,
                    schema,
                    strict,
                }
            }
        })
    }
}

/// One completion request to the configured model service. `purpose` names
/// the pass ("cleanup", "summary") for logs and usage accounting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmRequest {
    pub messages: Vec<LlmMessage>,
    pub response_format: LlmResponseFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i64>,
    pub purpose: String,
}

string_enum! {
    pub enum LlmFinishReason {
        Stop = "stop",
        Length = "length",
        ContentFilter = "contentFilter",
        Other = "other",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmResponse {
    pub text: String,
    pub finish_reason: LlmFinishReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<LlmUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::to_column_string;

    #[test]
    fn response_formats_use_the_case_coding_shape() {
        assert_eq!(
            to_column_string(&LlmResponseFormat::Text).unwrap(),
            r#""text""#
        );
        assert_eq!(
            to_column_string(&LlmResponseFormat::JsonObject).unwrap(),
            r#""jsonObject""#
        );
        let schema = LlmResponseFormat::JsonSchema {
            name: "summary".to_owned(),
            schema: serde_json::json!({"type": "object"}),
            strict: true,
        };
        let text = to_column_string(&schema).unwrap();
        assert_eq!(
            text,
            r#"{"jsonSchema":{"name":"summary","schema":{"type":"object"},"strict":true}}"#
        );
        assert_eq!(
            serde_json::from_str::<LlmResponseFormat>(&text).unwrap(),
            schema
        );
    }

    #[test]
    fn requests_and_responses_round_trip() {
        let request = LlmRequest {
            messages: vec![LlmMessage {
                role: LlmRole::User,
                content: "Hallo".to_owned(),
            }],
            response_format: LlmResponseFormat::Text,
            temperature: None,
            max_tokens: Some(100),
            purpose: "cleanup".to_owned(),
        };
        let text = to_column_string(&request).unwrap();
        assert_eq!(
            text,
            r#"{"maxTokens":100,"messages":[{"content":"Hallo","role":"user"}],"purpose":"cleanup","responseFormat":"text"}"#
        );
        assert_eq!(serde_json::from_str::<LlmRequest>(&text).unwrap(), request);
        let response = LlmResponse {
            text: "Hi".to_owned(),
            finish_reason: LlmFinishReason::ContentFilter,
            usage: Some(LlmUsage::ZERO),
            model: None,
        };
        let text = to_column_string(&response).unwrap();
        assert_eq!(
            text,
            r#"{"finishReason":"contentFilter","text":"Hi","usage":{"completionTokens":0,"promptTokens":0,"requests":0}}"#
        );
        assert_eq!(
            serde_json::from_str::<LlmResponse>(&text).unwrap(),
            response
        );
    }
}
