//! The wire formats: OpenAI chat completions, the Responses API as the
//! Codex backend speaks it, and server-sent events. Keys are spelled as the
//! APIs spell them; nothing here is stored and callers see
//! [`LlmRequest`](steno_core::LlmRequest) and
//! [`LlmResponse`](steno_core::LlmResponse).

mod chat;
mod responses;
mod sse;

pub use chat::{
    ChatChoice, ChatChoiceMessage, ChatCompletionRequest, ChatCompletionResponse, ChatErrorDetail,
    ChatErrorEnvelope, ChatMessage, ChatResponseFormat, ChatResponseSchema, ChatUsage, ModelList,
    ModelListEntry,
};
pub use responses::{
    CodexErrorEnvelope, CodexModel, CodexModelList, ResponsesError, ResponsesFormat,
    ResponsesIncompleteDetails, ResponsesInputItem, ResponsesInputPart, ResponsesOutputItem,
    ResponsesOutputPart, ResponsesReasoning, ResponsesRequest, ResponsesResponse,
    ResponsesStreamEvent, ResponsesText, ResponsesUsage,
};
pub use sse::{ServerSentEvent, looks_like_event_stream, parse_event_stream};

use serde::{Serialize, de::DeserializeOwned};

/// One JSON encoder for the wire: sorted keys and whole doubles as
/// integers (`steno_core::json::to_column_string`), so recorded request
/// bodies are byte-stable in tests and match what Swift's encoder wrote.
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    steno_core::json::to_column_string(value).map(String::into_bytes)
}

/// The one JSON decoder for the wire, the counterpart of [`encode`].
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// `value` as Swift's `JSONEncoder` prints it with `.prettyPrinted`,
/// `.sortedKeys` and `.withoutEscapingSlashes`: two-space indent, `" : "`
/// between key and value, and an empty container as an open and a close
/// bracket around a blank line. The reduce prompt carries the chunk notes
/// in this form, pinned by the golden. Keys and numbers are normalised by
/// the one-line encoder first, so both forms agree on them.
#[must_use]
pub fn swift_pretty(value: &serde_json::Value) -> String {
    let normalised: serde_json::Value = steno_core::json::to_column_string(value)
        .and_then(|text| serde_json::from_str(&text))
        .unwrap_or(serde_json::Value::Null);
    let mut out = String::new();
    write_pretty(&normalised, 0, &mut out);
    out
}

fn write_pretty(value: &serde_json::Value, indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    let inner = "  ".repeat(indent + 1);
    match value {
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{\n\n");
                out.push_str(&pad);
                out.push('}');
                return;
            }
            out.push_str("{\n");
            for (offset, (key, entry)) in map.iter().enumerate() {
                out.push_str(&inner);
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push_str(" : ");
                write_pretty(entry, indent + 1, out);
                if offset + 1 < map.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push('}');
        }
        serde_json::Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[\n\n");
                out.push_str(&pad);
                out.push(']');
                return;
            }
            out.push_str("[\n");
            for (offset, item) in items.iter().enumerate() {
                out.push_str(&inner);
                write_pretty(item, indent + 1, out);
                if offset + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}
