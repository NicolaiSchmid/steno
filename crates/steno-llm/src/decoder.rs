//! Turns a completion into a typed value.
//! Swift: `Sources/StenoLLM/StructuredOutput/StructuredOutputDecoder.swift`.

use serde::de::DeserializeOwned;
use steno_core::{LlmFinishReason, LlmResponse};

use crate::LlmError;

/// Turns a completion into a deserialisable value: `finish_reason: length`
/// is [`LlmError::Truncated`], a Markdown fence or prose around the JSON is
/// stripped, then serde decides. No repair heuristics: a failure is
/// [`LlmError::InvalidJson`] with the path named, and the caller's one
/// repair round takes over.
pub struct StructuredOutputDecoder;

impl StructuredOutputDecoder {
    pub fn decode<T: DeserializeOwned>(response: &LlmResponse) -> Result<T, LlmError> {
        if response.finish_reason == LlmFinishReason::Length {
            return Err(LlmError::Truncated);
        }
        let json = Self::extract_json(&response.text);
        if json.is_empty() {
            return Err(LlmError::InvalidJson("empty answer".to_owned()));
        }
        let mut deserializer = serde_json::Deserializer::from_str(&json);
        serde_path_to_error::deserialize(&mut deserializer)
            .map_err(|error| LlmError::InvalidJson(Self::describe(&error)))
    }

    /// The JSON inside `text`: the content of the first ``` fence when one
    /// opens before the JSON starts, else everything from the first `{` or
    /// `[` to the last matching `}` or `]`; whitespace trimmed. A fence
    /// after the first `{` or `[` is text inside the answer (a bullet
    /// quoting a code block), not Markdown around it.
    #[must_use]
    pub fn extract_json(text: &str) -> String {
        let mut body = text;
        let json_start = body.find(['{', '[']);
        if let Some(fence) = body.find("```")
            && json_start.is_none_or(|start| fence < start)
        {
            let mut after_fence = &body[fence + 3..];
            // Skip a language tag such as `json` up to the end of the line.
            if let Some(newline) = after_fence.find('\n') {
                let tag = &after_fence[..newline];
                if tag.chars().all(char::is_alphanumeric) {
                    after_fence = &after_fence[newline + 1..];
                }
            }
            // The last fence closes the block; an earlier one is quoted
            // content.
            body = match after_fence.rfind("```") {
                Some(closing) => &after_fence[..closing],
                None => after_fence,
            };
        }
        let Some(start) = body.find(['{', '[']) else {
            return body.trim().to_owned();
        };
        let closer = if body[start..].starts_with('{') {
            '}'
        } else {
            ']'
        };
        match body.rfind(closer) {
            Some(end) if end > start => body[start..=end].to_owned(),
            _ => body[start..].trim().to_owned(),
        }
    }

    /// The failure in Swift's `DecodingError` vocabulary with the path of
    /// the offending value: `missing key x at root`, `expected a sequence
    /// at sections.[0].bullets: …`, `malformed JSON at root: …`.
    #[must_use]
    pub fn describe(error: &serde_path_to_error::Error<serde_json::Error>) -> String {
        let path = Self::path(error.path());
        let inner = error.inner();
        let message = inner.to_string();
        // serde_json appends " at line L column C" to every message.
        let message = message
            .rfind(" at line ")
            .map_or(message.as_str(), |cut| &message[..cut])
            .to_owned();
        if matches!(
            inner.classify(),
            serde_json::error::Category::Syntax | serde_json::error::Category::Eof
        ) {
            return format!("malformed JSON at {path}: {message}");
        }
        if let Some(rest) = message.strip_prefix("missing field `")
            && let Some(key) = rest.strip_suffix('`')
        {
            return format!("missing key {key} at {path}");
        }
        if message.starts_with("invalid type")
            || message.starts_with("invalid value")
            || message.starts_with("invalid length")
        {
            let expected = message
                .rfind(", expected ")
                .map_or("another value", |cut| &message[cut + ", expected ".len()..]);
            return format!("expected {expected} at {path}: {message}");
        }
        format!("malformed JSON at {path}: {message}")
    }

    /// `sections.[0].bullets`, Swift's coding path rendering; `root` for the
    /// top level.
    fn path(path: &serde_path_to_error::Path) -> String {
        let segments: Vec<String> = path
            .iter()
            .map(|segment| match segment {
                serde_path_to_error::Segment::Seq { index } => format!("[{index}]"),
                serde_path_to_error::Segment::Map { key } => key.clone(),
                serde_path_to_error::Segment::Enum { variant } => variant.clone(),
                serde_path_to_error::Segment::Unknown => "?".to_owned(),
            })
            .collect();
        if segments.is_empty() {
            "root".to_owned()
        } else {
            segments.join(".")
        }
    }
}
