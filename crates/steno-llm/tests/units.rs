//! The pure pieces: the decoder, server-sent events and the Codex stream,
//! the token budget and chunker, the strict schema subset, retries,
//! redaction, the wire types and the endpoint from the settings.

#![allow(clippy::type_complexity)] // the case tables spell out their tuples

mod common;

use std::collections::HashMap;
use std::time::Duration;

use common::*;
use serde::Deserialize;
use serde_json::{Value, json};
use steno_core::{
    AudioLane, LanguageTag, LlmFinishReason, LlmProvider, LlmResponse, LlmUsage, Settings,
    SummaryTemplate, TranscriptSegment,
};
use steno_llm::testing::scripts;
use steno_llm::transport::{self, HttpReply};
use steno_llm::wire::{
    self, ChatCompletionRequest, ChatCompletionResponse, ChatErrorEnvelope, ChatMessage,
    ChatResponseFormat, ServerSentEvent, looks_like_event_stream, parse_event_stream,
};
use steno_llm::{
    CleanupDraft, CleanupPromptBuilder, CodexResponsesClient, JsonSchema, LlmEndpoint, LlmError,
    OpenAiCompatibleClient, RetryPolicy, StructuredOutputDecoder, StructuredOutputMode,
    SummaryPromptBuilder, TokenBudget, TranscriptChunk, TranscriptChunker,
};

fn response(text: &str, finish: LlmFinishReason) -> LlmResponse {
    LlmResponse {
        text: text.to_owned(),
        finish_reason: finish,
        usage: None,
        model: None,
    }
}

#[derive(Debug, PartialEq, Deserialize)]
struct Reply {
    ok: bool,
    items: Vec<String>,
}

#[test]
fn the_decoder_finds_the_json_in_fences_and_prose() {
    let cases = [
        ("{\"ok\":true,\"items\":[\"a\"]}", vec!["a"]),
        (
            "Sure! Here it is:\n```json\n{\"ok\": true, \"items\": []}\n```\nLet me know.",
            vec![],
        ),
        ("```json\n{\"ok\":true,\"items\":[]}", vec![]),
        (
            "The segments are: {\"ok\": true, \"items\": [\"x}\"]} — done.",
            vec!["x}"],
        ),
        // A fence marker inside a JSON string is content, not a fence.
        (
            "{\"ok\": true, \"items\": [\"Use ```swift``` blocks\"]}",
            vec!["Use ```swift``` blocks"],
        ),
        (
            "```json\n{\"ok\": true, \"items\": [\"```\"]}\n```",
            vec!["```"],
        ),
    ];
    for (text, items) in cases {
        let reply: Reply =
            StructuredOutputDecoder::decode(&response(text, LlmFinishReason::Stop)).unwrap();
        assert_eq!(reply.items, items, "{text}");
    }
    let numbers: Vec<i64> =
        StructuredOutputDecoder::decode(&response("Result: [1, 2, 3].", LlmFinishReason::Stop))
            .unwrap();
    assert_eq!(numbers, [1, 2, 3]);
}

#[test]
fn decode_failures_name_the_path_and_the_kind() {
    let decode = |text: &str, finish| {
        StructuredOutputDecoder::decode::<Reply>(&response(text, finish)).unwrap_err()
    };
    let stop = LlmFinishReason::Stop;
    let LlmError::InvalidJson(malformed) = decode("{\"ok\": true, \"items\": [\"a", stop) else {
        panic!()
    };
    assert!(malformed.starts_with("malformed JSON at"), "{malformed}");
    assert_eq!(
        decode("{\"ok\": true}", stop),
        LlmError::InvalidJson("missing key items at root".to_owned())
    );
    let LlmError::InvalidJson(wrong) = decode("{\"ok\": true, \"items\": [1]}", stop) else {
        panic!()
    };
    assert!(
        wrong.starts_with("expected a string at items.[0]"),
        "{wrong}"
    );
    assert_eq!(
        decode("{\"ok\":true,\"items\":[]}", LlmFinishReason::Length),
        LlmError::Truncated
    );
    assert_eq!(
        decode("   \n", stop),
        LlmError::InvalidJson("empty answer".to_owned())
    );
    // An empty object is a missing key, not an empty cleanup draft.
    assert_eq!(
        StructuredOutputDecoder::decode::<CleanupDraft>(&response("{}", stop)).unwrap_err(),
        LlmError::InvalidJson("missing key segments at root".to_owned())
    );
}

#[test]
fn server_sent_events_are_parsed_to_the_specification() {
    let named = |name: &str, data: &str| ServerSentEvent {
        event: Some(name.to_owned()),
        data: data.to_owned(),
    };
    let unnamed = |data: &str| ServerSentEvent {
        event: None,
        data: data.to_owned(),
    };
    let cases = [
        (
            ": comment\nevent: a\ndata: {\"b\":\ndata: 2}\nid: 7\n\ndata: x\n\n",
            vec![named("a", "{\"b\":\n2}"), unnamed("x")],
        ),
        ("event: a\r\ndata: {}\r\n", vec![named("a", "{}")]),
        ("event: a\rdata: {}\r\r", vec![named("a", "{}")]),
        // The last event needs no trailing blank line.
        (
            "data: a\n\nevent: b\ndata: {}",
            vec![unnamed("a"), named("b", "{}")],
        ),
        ("event: a\n\n: keep-alive\n\nretry: 100\n\n", vec![]),
        ("event: a\n\ndata: x\n\n", vec![unnamed("x")]),
        ("data\n\ndata:{}\n\n", vec![unnamed(""), unnamed("{}")]),
        ("data:  two\n\n", vec![unnamed(" two")]),
        (
            "data: {\"k\": \"a:b\"}\n\n",
            vec![unnamed("{\"k\": \"a:b\"}")],
        ),
    ];
    for (body, events) in cases {
        assert_eq!(parse_event_stream(body), events, "{body:?}");
    }
    assert!(looks_like_event_stream(
        Some("text/event-stream; charset=utf-8"),
        b""
    ));
    assert!(looks_like_event_stream(None, b"data: {}\n\n"));
    assert!(!looks_like_event_stream(Some("application/json"), b"{}"));
    assert!(!looks_like_event_stream(None, b"{\"id\":1}"));
}

#[test]
fn a_codex_stream_becomes_one_response() {
    let usage = |prompt, completion| {
        Some(LlmUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            requests: 1,
        })
    };
    let ok = |text: &str, finish| Ok((text.to_owned(), finish));
    let stream = |text, status, reason| scripts.responses_stream(text, status, reason, None, "m");
    let multi_item = r#"data: {"type":"response.created","response":{"id":"r","status":"in_progress"}}

data: {"type":"response.output_item.done","item":{"type":"reasoning","id":"rs","content":[{"type":"output_text","text":"thinking"}]}}

data: {"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"{\"ok\":"}]}}

event: some.future.event
data: {"type":"some.future.event"}

data: [DONE]

data: {"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"true}"}]}}

data: {"type":"response.completed","response":{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"not this"}]}]}}

"#;
    let output_only = "event: response.completed\ndata: {\"type\":\"x\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"from output\"}]}]}}";
    let cases: Vec<(Vec<u8>, Result<(String, LlmFinishReason), LlmError>)> = vec![
        (scripts.stream("hi").body, ok("hi", LlmFinishReason::Stop)),
        (
            stream("cut", "incomplete", Some("max_output_tokens")).body,
            ok("cut", LlmFinishReason::Length),
        ),
        (
            multi_item.into(),
            ok("{\"ok\":true}", LlmFinishReason::Stop),
        ),
        (output_only.into(), ok("from output", LlmFinishReason::Stop)),
        (
            scripts.responses_failed("boom").body,
            Err(LlmError::Transport("boom".to_owned())),
        ),
        (
            scripts.responses_refusal("no").body,
            Err(LlmError::Refused("no".to_owned())),
        ),
        (
            scripts.responses_truncated_stream().body,
            Err(LlmError::Transport(
                "stream closed before response.completed".to_owned(),
            )),
        ),
        (
            scripts
                .responses_error_event("acct_123 said no", "server_error")
                .body,
            Err(LlmError::Transport("[redacted] said no".to_owned())),
        ),
        (
            scripts
                .responses_error_event("full", "context_length_exceeded")
                .body,
            Err(LlmError::Http {
                status: 400,
                body: "context_length_exceeded: full".to_owned(),
            }),
        ),
    ];
    let secrets = ["acct_123".to_owned()];
    for (body, expected) in cases {
        let parsed =
            CodexResponsesClient::parse_stream(&body, &secrets).map(|r| (r.text, r.finish_reason));
        assert_eq!(parsed, expected, "{}", String::from_utf8_lossy(&body));
    }
    let full = scripts.responses_stream("{}", "completed", None, usage(43, 12), "gpt-x");
    let parsed = CodexResponsesClient::parse_stream(&full.body, &[]).unwrap();
    assert_eq!(parsed.usage, usage(43, 12));
    assert_eq!(parsed.model.as_deref(), Some("gpt-x"));
    let parsed = CodexResponsesClient::parse_stream(multi_item.as_bytes(), &[]).unwrap();
    assert_eq!(parsed.usage, usage(0, 0), "no usage still counts a request");

    // A plain JSON reply is read whole; anything else is a transport error.
    let reply = |body: &str| HttpReply {
        status: 200,
        headers: HashMap::new(),
        body: body.as_bytes().to_vec(),
    };
    let whole = CodexResponsesClient::parse(
        &reply(r#"{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"message","content":[{"type":"output_text","text":"whole"}]}]}"#),
        &[],
    )
    .unwrap();
    assert_eq!(
        (whole.text.as_str(), whole.finish_reason),
        ("whole", LlmFinishReason::Length)
    );
    assert_eq!(
        CodexResponsesClient::parse(&reply("<html>"), &[]).unwrap_err(),
        LlmError::Transport("undecodable response body: <html>".to_owned())
    );
}

#[test]
fn tokens_are_estimated_from_bytes_per_language() {
    let text = fixture_text("text/de-1000-words.txt");
    assert_eq!(text.split_whitespace().count(), 1_000);
    let german = TokenBudget::estimate_tokens(&text, Some(&de()));
    assert!((1_200..=2_500).contains(&german), "{german}");
    // 3.6 bytes a token for English, 3.0 for every other or no language.
    let english = "The quick brown fox jumps over the lazy dog. ".repeat(20);
    assert_eq!(english.len(), 900);
    for (text, tag, tokens) in [
        (english.as_str(), Some("en-US"), 250),
        (english.as_str(), Some("EN"), 250),
        (english.as_str(), Some("fr"), 300),
        (english.as_str(), None, 300),
        ("", Some("de"), 0),
        ("ä", Some("de"), 1),
    ] {
        let tag = tag.map(LanguageTag::from);
        assert_eq!(
            TokenBudget::estimate_tokens(text, tag.as_ref()),
            tokens,
            "{tag:?}"
        );
    }
}

#[test]
fn budgets_follow_the_context_within_floors_and_ceilings() {
    let budget = |reserved, overhead| TokenBudget {
        context_tokens: 8_000,
        reserved_output_tokens: reserved,
        prompt_overhead_tokens: overhead,
    };
    assert_eq!(budget(2_000, 1_500).input_budget(), 4_500);
    assert!(budget(2_000, 1_500).fits(4_500) && !budget(2_000, 1_500).fits(4_501));
    assert_eq!(budget(6_000, 3_000).input_budget(), 0, "never negative");
    let map = budget(2_000, 1_211);
    for (chunks, notes) in [(10, 478), (1, 1_500), (0, 1_500), (100, 256)] {
        assert_eq!(
            map.map_notes_output_tokens(chunks),
            notes,
            "{chunks} chunks"
        );
    }
    for (notes, points) in [(1_500, 25), (478, 7), (100, 3)] {
        assert_eq!(
            SummaryPromptBuilder::<chrono::Utc>::max_notes_points(notes),
            points
        );
    }
    let endpoint = |context, output| LlmEndpoint {
        context_tokens: context,
        max_output_tokens: output,
        ..LlmEndpoint::new(url::Url::parse("http://127.0.0.1:1234/v1").unwrap(), "m")
    };
    for (context, output, chunk, summary) in [
        (32_000, 4_096, 15_488, 4_096),
        (8_000, 4_096, 3_488, 2_000),
        (8_000, 1_000, 3_488, 1_000),
        (1_024, 4_096, 256, 256),
    ] {
        let endpoint = endpoint(context, output);
        assert_eq!(endpoint.cleanup_chunk_budget_tokens(), chunk, "{context}");
        assert_eq!(
            endpoint.summary_reserved_output_tokens(),
            summary,
            "{context}"
        );
    }
    // The cleanup answer repeats its chunk with headroom, under the ceiling.
    let builder = CleanupPromptBuilder::new(1_000);
    let call = customer_call();
    let big = &TranscriptChunker::default().chunk(&call.segments, Some(&de()))[0];
    assert_eq!(builder.output_tokens(big, Some(&de())), 1_000);
    let small = &TranscriptChunker::new(50, 80, 3).chunk(&standup().segments, Some(&de()))[0];
    assert!((256..1_000).contains(&builder.output_tokens(small, Some(&de()))));
}

fn segment(index: u32, words: usize, speaker: uuid::Uuid) -> TranscriptSegment {
    let text = vec!["wort"; words].join(" ");
    TranscriptSegment {
        id: sample_uuid(300 + index),
        meeting_id: sample_uuid(1),
        start: f64::from(index),
        end: f64::from(index + 1),
        speaker_id: Some(speaker),
        lane: AudioLane::Mixed,
        text: text.clone(),
        raw_text: text,
    }
}

/// Order, completeness, the `max_tokens` bound, the close rule and the
/// leading context.
fn check_packing(
    chunks: &[TranscriptChunk],
    segments: &[TranscriptSegment],
    chunker: &TranscriptChunker,
    label: &str,
) {
    let flattened: Vec<&TranscriptSegment> = chunks.iter().flat_map(|c| &c.segments).collect();
    assert!(
        flattened.iter().copied().eq(segments.iter()),
        "{label}: order"
    );
    for (index, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk.index, index, "{label}");
        assert!(
            chunk.estimated_tokens <= chunker.max_tokens || chunk.segments.len() == 1,
            "{label}: chunk {index} too big"
        );
    }
    for pair in chunks.windows(2) {
        let (previous, chunk) = (&pair[0], &pair[1]);
        let skip = previous
            .segments
            .len()
            .saturating_sub(chunker.context_segments);
        assert_eq!(chunk.leading_context, previous.segments[skip..], "{label}");
        let next = &chunk.segments[0];
        let overflow = previous.estimated_tokens
            + TranscriptChunker::estimate_segment(next, Some(&de()))
            > chunker.max_tokens;
        let turn = previous.estimated_tokens >= chunker.target_tokens
            && Some(next.speaker_id) != previous.segments.last().map(|s| s.speaker_id);
        assert!(
            overflow || turn,
            "{label}: chunk {} closed early",
            previous.index
        );
    }
}

#[test]
fn the_chunker_splits_on_speaker_turns_within_the_budget() {
    let call = customer_call();
    let chunks = TranscriptChunker::default().chunk(&call.segments, Some(&de()));
    assert!((6..=12).contains(&chunks.len()), "{} chunks", chunks.len());
    for (target, max, context) in [(50, 80, 3), (150, 220, 2), (400, 400, 0), (2_000, 3_000, 3)] {
        for count in [1, 7, 300, 900] {
            let chunker = TranscriptChunker::new(target, max, context);
            let segments = &call.segments[..count];
            let chunks = chunker.chunk(segments, Some(&de()));
            check_packing(
                &chunks,
                segments,
                &chunker,
                &format!("{target}/{max}/{count}"),
            );
        }
    }
    // An oversized segment gets a chunk of its own.
    let (a, b) = (sample_uuid(410), sample_uuid(411));
    let segments = [
        segment(0, 10, a),
        segment(1, 10, b),
        segment(2, 2_000, a),
        segment(3, 10, b),
    ];
    let chunker = TranscriptChunker::new(50, 80, 3);
    let chunks = chunker.chunk(&segments, Some(&de()));
    let sizes: Vec<usize> = chunks.iter().map(|c| c.segments.len()).collect();
    assert_eq!(sizes, [2, 1, 1]);
    check_packing(&chunks, &segments, &chunker, "oversized");
    assert_eq!(TranscriptChunker::default().chunk(&[], None).len(), 0);
    let clamped = TranscriptChunker::with_budget(500, 3);
    assert_eq!((clamped.target_tokens, clamped.max_tokens), (500, 500));
    let roomy = TranscriptChunker::with_budget(10_000, 3);
    assert_eq!((roomy.target_tokens, roomy.max_tokens), (2_000, 3_000));
}

/// Fails unless `value` keeps to the strict structured output subset: every
/// object closed with every property required, no other keywords.
fn assert_strict(value: &Value, path: &str) {
    let allowed = [
        "type",
        "properties",
        "required",
        "additionalProperties",
        "items",
        "enum",
        "description",
    ];
    let object = value.as_object().unwrap();
    assert!(
        object.keys().all(|key| allowed.contains(&key.as_str())),
        "{path}: {value}"
    );
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        assert_eq!(object["additionalProperties"], false, "{path}");
        let mut required: Vec<&str> = object["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        required.sort_unstable();
        let mut names: Vec<&str> = properties.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(required, names, "{path}: every property required");
        for (name, child) in properties {
            assert_strict(child, &format!("{path}.{name}"));
        }
    }
    if let Some(items) = object.get("items") {
        assert_strict(items, &format!("{path}[]"));
    }
}

#[test]
fn every_schema_sent_is_in_the_strict_subset() {
    let template = SummaryTemplate::bundled_with_id("interview").unwrap();
    let builder = SummaryPromptBuilder::new(template.clone(), chrono::Utc);
    let sample = JsonSchema::object(vec![
        ("score", JsonSchema::number().nullable()),
        ("kind", JsonSchema::string_enum(&["a", "b"])),
    ]);
    assert_eq!(
        sample.json_value()["properties"]["score"]["type"],
        json!(["number", "null"])
    );
    for (name, schema) in [
        ("sample", sample.json_value()),
        ("probe", OpenAiCompatibleClient::probe_schema().json_value()),
        (
            "cleanup",
            CleanupPromptBuilder::output_schema().json_value(),
        ),
        ("draft", builder.draft_schema().json_value()),
        ("notes", builder.notes_schema().json_value()),
    ] {
        assert_strict(&schema, name);
    }
    let draft = builder.draft_schema().json_value();
    let ids: Vec<Value> = template.sections.iter().map(|s| json!(s.id)).collect();
    assert_eq!(
        draft["properties"]["sections"]["items"]["properties"]["id"]["enum"],
        Value::Array(ids)
    );
}

#[test]
fn retries_back_off_exponentially_and_only_for_transient_failures() {
    let policy = RetryPolicy::default();
    assert_eq!(policy.max_attempts, 3);
    for (retry, after, delay) in [
        (1, None, 2),
        (2, None, 4),
        (3, None, 8),
        (10, None, 30),
        (1, Some(7), 7),
        (1, Some(600), 30),
    ] {
        assert_eq!(
            policy.delay(retry, after.map(secs)),
            secs(delay),
            "{retry} {after:?}"
        );
    }
    assert_eq!(RetryPolicy::with_max_attempts(0).max_attempts, 1);
    for (header, expected) in [
        (Some("7"), Some(secs(7))),
        (Some(" 1.5 "), Some(Duration::from_millis(1_500))),
        (Some("Wed, 21 Oct 2026 07:28:00 GMT"), None),
        (Some("inf"), None),
        (Some("nan"), None),
        (Some("-1"), None),
        (Some("1e300"), Some(secs(3_600))),
        (Some("3601"), Some(secs(3_600))),
        (None, None),
    ] {
        assert_eq!(transport::retry_after(header), expected, "{header:?}");
    }
    let http = |status| LlmError::Http {
        status,
        body: String::new(),
    };
    for status in [408, 500, 503, 599] {
        assert!(http(status).is_retryable(), "{status}");
    }
    for status in [400, 401, 404, 429, 200] {
        assert!(!http(status).is_retryable(), "{status}");
    }
    for error in [
        LlmError::RateLimited { retry_after: None },
        LlmError::Transport(String::new()),
        LlmError::Timeout,
    ] {
        assert!(
            error.is_retryable() && !error.is_answer_problem(),
            "{error}"
        );
    }
    for error in [
        LlmError::InvalidJson(String::new()),
        LlmError::Truncated,
        LlmError::Refused(String::new()),
    ] {
        assert!(
            !error.is_retryable() && error.is_answer_problem(),
            "{error}"
        );
    }
    assert_eq!(transport::wall_clock_backstop(secs(240)), secs(480));
    assert_eq!(transport::wall_clock_backstop(secs(5)), secs(30));
}

#[test]
fn redaction_removes_every_form_of_a_secret_before_the_cut() {
    let key = API_KEY.to_owned();
    let outer = format!("{API_KEY}-suffix");
    let cases: Vec<(&str, Vec<String>, String)> = vec![
        (
            "every copy",
            vec![key.clone()],
            format!("{API_KEY} a {API_KEY}"),
        ),
        (
            "nested secrets, either order",
            vec![key.clone(), outer.clone()],
            format!("{outer} {API_KEY}"),
        ),
        (
            "nested secrets, either order",
            vec![outer.clone(), key.clone()],
            format!("{outer} {API_KEY}"),
        ),
        (
            "JSON-escaped",
            vec![r#"sk-test/AB"CD\EFGH12"#.to_owned()],
            r#"sk-test/AB\"CD\\EFGH12 sk-test\/AB\"CD\\EFGH12"#.to_owned(),
        ),
    ];
    for (what, secrets, text) in cases {
        let redacted = transport::redact(&text, &secrets);
        assert!(!redacted.contains("sk-test"), "{what}: {redacted}");
        assert!(redacted.contains("[redacted]"), "{what}");
    }
    // Shorter than eight bytes is a placeholder, not a credential.
    assert_eq!(
        transport::redact("model x ollama", &["x".into(), "ollama".into()]),
        "model x ollama"
    );
    assert_eq!(
        transport::redact("too exceeded", &["exceeded".into()]),
        "too [redacted]"
    );

    // Seeded property: random secrets at random places, cut anywhere after
    // multi-byte filler; no eight bytes of a secret survive the cut.
    let mut seed: u64 = 0x5EED;
    let mut next = |n: usize| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(seed >> 33).unwrap() % n
    };
    for _ in 0..300 {
        let secrets: Vec<String> = (0..=next(3))
            .map(|i| format!("sk-{i}-{}", "aB3/\"x".repeat(2 + next(8))))
            .collect();
        let mut body = String::new();
        while body.len() < 600 {
            match next(4) {
                0 => body.push_str(&secrets[next(secrets.len())]),
                1 => body.push('€'),
                2 => body.push_str(&serde_json::to_string(&secrets[next(secrets.len())]).unwrap()),
                _ => body.push_str("filler "),
            }
        }
        let limit = next(body.chars().count());
        let cut = transport::redacted_prefix(&body, &secrets, limit);
        assert!(cut.chars().count() <= limit);
        for secret in &secrets {
            for piece in secret.as_bytes().windows(8) {
                let piece = std::str::from_utf8(piece).unwrap();
                assert!(!cut.contains(piece), "{piece} survived in {cut}");
            }
        }
    }
}

#[test]
fn wire_types_round_trip_through_their_snake_case_keys() {
    let request = ChatCompletionRequest {
        model: "m".to_owned(),
        messages: vec![ChatMessage {
            role: "system".to_owned(),
            content: "s".to_owned(),
        }],
        temperature: Some(0.2),
        max_tokens: Some(100),
        max_completion_tokens: None,
        response_format: Some(ChatResponseFormat::json_schema(
            "n",
            json!({"type": "object"}),
            true,
        )),
    };
    let encoded = String::from_utf8(wire::encode(&request).unwrap()).unwrap();
    assert!(encoded.contains("\"max_tokens\":100"), "{encoded}");
    assert!(
        encoded.contains("\"response_format\":{\"json_schema\":{\"name\":\"n\""),
        "{encoded}"
    );
    assert!(!encoded.contains("max_completion_tokens"));
    assert_eq!(
        wire::decode::<ChatCompletionRequest>(encoded.as_bytes()).unwrap(),
        request
    );
    let parts = r#"{"choices":[{"message":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}}]}"#;
    let reply: ChatCompletionResponse = wire::decode(parts.as_bytes()).unwrap();
    assert_eq!(reply.choices[0].message.content.as_deref(), Some("ab"));
    let error: ChatErrorEnvelope =
        wire::decode(br#"{"error":{"message":"m","code":400}}"#).unwrap();
    assert_eq!(error.error.code, Some(json!(400)));
    // The mode's spelling is shared with the settings file.
    for (mode, text) in [
        (StructuredOutputMode::JsonSchema, "jsonSchema"),
        (StructuredOutputMode::JsonObject, "jsonObject"),
        (StructuredOutputMode::PromptOnly, "promptOnly"),
    ] {
        assert_eq!(serde_json::to_string(&mode).unwrap(), format!("\"{text}\""));
        assert_eq!(text.parse::<StructuredOutputMode>().unwrap(), mode);
    }
}

/// An endpoint needs a parsing URL and a model; Codex needs a model and
/// the confirmed credential use. The context is clamped to 1 024.
#[test]
fn the_endpoint_comes_from_the_settings() {
    fn local(s: &mut Settings) {
        s.llm_base_url = Some("http://127.0.0.1:1234/v1/".to_owned());
        s.llm_model = Some("local".to_owned());
        s.llm_context_tokens = 16_000;
    }
    fn codex(s: &mut Settings) {
        s.llm_provider = LlmProvider::Codex;
        s.codex_model = Some("gpt-5.6-terra".to_owned());
        s.codex_confirmed_at = Some(codex_now());
    }
    type Edit = fn(&mut Settings);
    let cases: [(&str, Edit, Option<(&str, i64, bool)>); 8] = [
        ("nothing set", |_| {}, None),
        (
            "endpoint",
            local,
            Some(("http://127.0.0.1:1234/v1/chat/completions", 16_000, false)),
        ),
        (
            "empty model",
            |s| {
                local(s);
                s.llm_model = Some(String::new());
            },
            None,
        ),
        (
            "bad URL",
            |s| {
                local(s);
                s.llm_base_url = Some("not a url".into());
            },
            None,
        ),
        (
            "tiny context",
            |s| {
                local(s);
                s.llm_context_tokens = 10;
            },
            Some(("http://127.0.0.1:1234/v1/chat/completions", 1_024, false)),
        ),
        (
            "Codex",
            |s| {
                local(s);
                codex(s);
                s.codex_context_tokens = 10;
            },
            Some((
                "https://chatgpt.com/backend-api/codex/responses",
                1_024,
                true,
            )),
        ),
        (
            "Codex unconfirmed",
            |s| {
                codex(s);
                s.codex_confirmed_at = None;
            },
            None,
        ),
        // Pasting the backend's address stays an endpoint, so the
        // confirmation cannot be walked around.
        (
            "pasted Codex URL",
            |s| {
                local(s);
                s.llm_base_url = Some(LlmEndpoint::CODEX_BACKEND_URL.into());
            },
            Some((
                "https://chatgpt.com/backend-api/codex/chat/completions",
                16_000,
                false,
            )),
        ),
    ];
    for (what, edit, expected) in cases {
        let mut settings = Settings::default();
        edit(&mut settings);
        let found = LlmEndpoint::from_settings(&settings).map(|e| {
            let url = if e.is_codex_backend() {
                e.responses_url()
            } else {
                e.chat_completions_url()
            };
            (url.to_string(), e.context_tokens, e.is_codex_backend())
        });
        assert_eq!(
            found,
            expected.map(|(url, context, codex)| (url.to_owned(), context, codex)),
            "{what}"
        );
    }
}

/// Without this a client built outside `http_client_builder` panics at
/// its first `https` request.
#[test]
fn the_http_client_builder_installs_a_tls_provider() {
    let _client = transport::http_client_builder().build().unwrap();
    assert!(rustls::crypto::CryptoProvider::get_default().is_some());
}
