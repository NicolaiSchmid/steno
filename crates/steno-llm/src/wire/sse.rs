//! A complete `text/event-stream` body as a list of events.
//! Swift: `Sources/StenoLLM/Wire/ServerSentEvents.swift`.

/// One server-sent event: the `event:` name when the server sent one and
/// the `data:` lines joined with newlines, as the specification reads them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSentEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Parses a complete `text/event-stream` body. Steno buffers the whole
/// answer (nothing shows partial output), so this is a pure function over
/// the bytes: events are separated by a blank line, lines end in `\n`,
/// `\r\n` or `\r`, a line starting with `:` is a comment, `id:` and `retry:`
/// are ignored, and an event without data is dropped.
#[must_use]
pub fn parse_event_stream(text: &str) -> Vec<ServerSentEvent> {
    let mut events = Vec::new();
    let mut name: Option<String> = None;
    let mut lines: Vec<String> = Vec::new();
    let normalised = text.replace("\r\n", "\n").replace('\r', "\n");

    let mut flush = |name: &mut Option<String>, lines: &mut Vec<String>| {
        if !lines.is_empty() {
            events.push(ServerSentEvent {
                event: name.take(),
                data: lines.join("\n"),
            });
        }
        *name = None;
        lines.clear();
    };

    for line in normalised.split('\n') {
        if line.is_empty() {
            flush(&mut name, &mut lines);
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.find(':') {
            Some(colon) => {
                let value = &line[colon + 1..];
                (&line[..colon], value.strip_prefix(' ').unwrap_or(value))
            }
            None => (line, ""),
        };
        match field {
            "event" => name = Some(value.to_owned()),
            "data" => lines.push(value.to_owned()),
            _ => {}
        }
    }
    flush(&mut name, &mut lines);
    events
}

/// Whether `body` reads as an event stream: a `Content-Type` of
/// `text/event-stream`, or a body whose first line is a field.
#[must_use]
pub fn looks_like_event_stream(content_type: Option<&str>, body: &[u8]) -> bool {
    if content_type.is_some_and(|value| value.to_lowercase().contains("text/event-stream")) {
        return true;
    }
    let head = String::from_utf8_lossy(&body[..body.len().min(64)]);
    let first_line = head.split('\n').next().unwrap_or("");
    first_line.starts_with("event:")
        || first_line.starts_with("data:")
        || first_line.starts_with(':')
}
