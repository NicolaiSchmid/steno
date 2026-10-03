//! The pure pieces: the structured output decoder, the SSE parser, the token
//! budget, the chunker, the strict schema subset and the stub server
//! itself. Swift: `StructuredDecoderTests`, `ServerSentEventsTests`,
//! `TokenBudgetTests`, `TranscriptChunkerTests`, `ChunkerPropertyTests`,
//! `JSONSchemaStrictTests`, `StubChatServerTests`, `LLMFixturesTests`.

mod common;

use common::*;
use serde::Deserialize;
use serde_json::{Value, json};
use steno_core::{
    AudioLane, LanguageTag, LlmFinishReason, LlmResponse, LlmUsage, TranscriptSegment,
};
use steno_llm::budget::primary_subtag;
use steno_llm::testing::{StubChatServer, scripts};
use steno_llm::wire::{
    self, ChatCompletionRequest, ChatCompletionResponse, ChatErrorEnvelope, ChatMessage,
    ChatResponseFormat, ModelList, ServerSentEvent, looks_like_event_stream, parse_event_stream,
};
use steno_llm::{
    BudgetPolicy, CleanupDraft, CodexResponsesClient, JsonSchema, LlmEndpoint, LlmError,
    OpenAiCompatibleClient, StructuredOutputDecoder, StructuredOutputMode, SummaryPromptBuilder,
    TokenBudget, TranscriptChunk, TranscriptChunker,
};

fn response(text: &str, finish: LlmFinishReason) -> LlmResponse {
    LlmResponse {
        text: text.to_owned(),
        finish_reason: finish,
        usage: None,
        model: None,
    }
}

// Structured output decoder

#[derive(Debug, PartialEq, Deserialize)]
struct Reply {
    ok: bool,
    items: Vec<String>,
}

fn decode(text: &str) -> Result<Reply, LlmError> {
    StructuredOutputDecoder::decode(&response(text, LlmFinishReason::Stop))
}

#[test]
fn decodes_bare_json_and_strips_fences_and_prose() {
    assert_eq!(
        decode("{\"ok\":true,\"items\":[\"a\"]}").unwrap(),
        Reply {
            ok: true,
            items: vec!["a".to_owned()]
        }
    );
    let fenced = "Sure! Here it is:\n```json\n{\"ok\": true, \"items\": []}\n```\nLet me know.";
    assert_eq!(
        StructuredOutputDecoder::extract_json(fenced),
        "{\"ok\": true, \"items\": []}"
    );
    assert!(decode(fenced).unwrap().ok);
    assert_eq!(
        StructuredOutputDecoder::extract_json("```\n{\"ok\":false,\"items\":[]}\n```"),
        "{\"ok\":false,\"items\":[]}"
    );
    assert_eq!(
        StructuredOutputDecoder::extract_json("```json\n{\"ok\":false,\"items\":[]}"),
        "{\"ok\":false,\"items\":[]}"
    );
    let prose = "The cleaned segments are: {\"ok\": true, \"items\": [\"x}\"]} — done.";
    assert_eq!(
        StructuredOutputDecoder::extract_json(prose),
        "{\"ok\": true, \"items\": [\"x}\"]}"
    );
    assert_eq!(decode(prose).unwrap().items, ["x}"]);
    assert_eq!(
        StructuredOutputDecoder::extract_json("Result: [1, 2, 3]."),
        "[1, 2, 3]"
    );
    let numbers: Vec<i64> =
        StructuredOutputDecoder::decode(&response("Result: [1, 2, 3].", LlmFinishReason::Stop))
            .unwrap();
    assert_eq!(numbers, [1, 2, 3]);
}

/// A fence marker inside a JSON string (a bullet quoting a code block) is
/// content, not a Markdown fence around the answer.
#[test]
fn a_fence_inside_the_json_is_not_a_fence() {
    let text = "{\"ok\": true, \"items\": [\"Use ```swift``` blocks\"]}";
    assert_eq!(StructuredOutputDecoder::extract_json(text), text);
    assert_eq!(decode(text).unwrap().items, ["Use ```swift``` blocks"]);
    let fenced_and_quoted = "```json\n{\"ok\": false, \"items\": [\"```\"]}\n```";
    assert_eq!(
        StructuredOutputDecoder::extract_json(fenced_and_quoted),
        "{\"ok\": false, \"items\": [\"```\"]}"
    );
}

#[test]
fn decode_failures_name_the_path_and_the_kind() {
    match decode("{\"ok\": true, \"items\": [\"a\", \"b").unwrap_err() {
        LlmError::InvalidJson(detail) => assert!(detail.contains("malformed JSON"), "{detail}"),
        other => panic!("{other}"),
    }
    assert_eq!(
        decode("{\"ok\": true}").unwrap_err(),
        LlmError::InvalidJson("missing key items at root".to_owned())
    );
    match decode("{\"ok\": true, \"items\": [1]}").unwrap_err() {
        LlmError::InvalidJson(detail) => {
            assert!(
                detail.starts_with("expected a string at items.[0]"),
                "{detail}"
            );
        }
        other => panic!("{other}"),
    }
    assert_eq!(
        StructuredOutputDecoder::decode::<Reply>(&response(
            "{\"ok\":true,\"items\":[]}",
            LlmFinishReason::Length
        ))
        .unwrap_err(),
        LlmError::Truncated
    );
    assert_eq!(
        decode("   \n").unwrap_err(),
        LlmError::InvalidJson("empty answer".to_owned())
    );
}

// Server-sent events

#[test]
fn parses_named_events_and_multiline_data() {
    let body = ": a comment the client ignores\nevent: response.created\ndata: {\"a\":1}\n\nevent: response.output_text.delta\ndata: {\"b\":\ndata: 2}\nid: 7\n\ndata: {\"unnamed\":true}\n\n";
    assert_eq!(
        parse_event_stream(body),
        [
            ServerSentEvent {
                event: Some("response.created".to_owned()),
                data: "{\"a\":1}".to_owned()
            },
            ServerSentEvent {
                event: Some("response.output_text.delta".to_owned()),
                data: "{\"b\":\n2}".to_owned()
            },
            ServerSentEvent {
                event: None,
                data: "{\"unnamed\":true}".to_owned()
            },
        ]
    );
}

#[test]
fn accepts_crlf_and_the_specifications_corners() {
    let completed = |data: &str| ServerSentEvent {
        event: Some("response.completed".to_owned()),
        data: data.to_owned(),
    };
    assert_eq!(
        parse_event_stream("event: response.completed\r\ndata: {}\r\n"),
        [completed("{}")]
    );
    assert_eq!(
        parse_event_stream("event: a\rdata: {}\r\r"),
        [ServerSentEvent {
            event: Some("a".to_owned()),
            data: "{}".to_owned()
        }]
    );
    assert_eq!(parse_event_stream("").len(), 0);
    assert_eq!(parse_event_stream("event: response.completed\n\n").len(), 0);
    assert_eq!(
        parse_event_stream(": keep-alive\n\nid: 3\nretry: 100\n\n").len(),
        0
    );
    let unnamed = |data: &str| ServerSentEvent {
        event: None,
        data: data.to_owned(),
    };
    assert_eq!(
        parse_event_stream("event: a\n\ndata: x\n\n"),
        [unnamed("x")]
    );
    assert_eq!(parse_event_stream("data\n\n"), [unnamed("")]);
    assert_eq!(parse_event_stream("data:{}\n\n"), [unnamed("{}")]);
    assert_eq!(parse_event_stream("data:  two\n\n"), [unnamed(" two")]);
    assert_eq!(
        parse_event_stream("data: {\"k\": \"a:b\"}\n\n"),
        [unnamed("{\"k\": \"a:b\"}")],
        "only the first colon splits the field"
    );
}

#[test]
fn a_stream_that_ends_without_a_blank_line_keeps_its_last_event() {
    let completed = || ServerSentEvent {
        event: Some("response.completed".to_owned()),
        data: "{}".to_owned(),
    };
    assert_eq!(
        parse_event_stream("event: response.completed\ndata: {}"),
        [completed()]
    );
    assert_eq!(
        parse_event_stream("event: response.completed\r\ndata: {}"),
        [completed()]
    );
    assert_eq!(
        parse_event_stream("data: a\n\nevent: response.completed\ndata: {}"),
        [
            ServerSentEvent {
                event: None,
                data: "a".to_owned()
            },
            completed()
        ]
    );
}

#[test]
fn a_cleanup_reply_without_segments_is_invalid_json_not_an_empty_draft() {
    for text in ["{}", "{\"segment\": []}", "```json\n{}\n```"] {
        assert_eq!(
            StructuredOutputDecoder::decode::<CleanupDraft>(&response(text, LlmFinishReason::Stop))
                .unwrap_err(),
            LlmError::InvalidJson("missing key segments at root".to_owned()),
            "{text}"
        );
    }
}

#[test]
fn detects_an_event_stream_by_header_or_shape() {
    assert!(looks_like_event_stream(
        Some("text/event-stream; charset=utf-8"),
        b""
    ));
    assert!(looks_like_event_stream(None, b"event: x\ndata: {}\n\n"));
    assert!(looks_like_event_stream(None, b"data: {}\n\n"));
    assert!(!looks_like_event_stream(Some("application/json"), b"{}"));
    assert!(!looks_like_event_stream(None, b"{\"id\":1}"));
}

#[test]
fn a_stream_becomes_one_response() {
    let stub = scripts.responses_stream(
        "{\"ok\":true}",
        "completed",
        None,
        Some(LlmUsage {
            prompt_tokens: 43,
            completion_tokens: 12,
            requests: 1,
        }),
        "gpt-5.6-luna",
    );
    let response = CodexResponsesClient::parse_stream(&stub.body, &[]).unwrap();
    assert_eq!(response.text, "{\"ok\":true}");
    assert_eq!(response.finish_reason, LlmFinishReason::Stop);
    assert_eq!(
        response.usage,
        Some(LlmUsage {
            prompt_tokens: 43,
            completion_tokens: 12,
            requests: 1
        })
    );
    assert_eq!(response.model.as_deref(), Some("gpt-5.6-luna"));
}

#[test]
fn a_codex_stream_without_a_trailing_blank_line_still_completes() {
    let stub = scripts.responses_stream("{\"ok\":true}", "completed", None, None, "m");
    let body = std::str::from_utf8(&stub.body).unwrap().trim_end();
    assert!(body.ends_with('}'), "the terminal event is the last line");
    let response = CodexResponsesClient::parse_stream(body.as_bytes(), &[]).unwrap();
    assert_eq!(response.text, "{\"ok\":true}");
    assert_eq!(response.finish_reason, LlmFinishReason::Stop);
}

#[test]
fn incomplete_failed_refused_and_cut_streams() {
    let parse = |stub: steno_llm::testing::StubResponse, secrets: &[String]| {
        CodexResponsesClient::parse_stream(&stub.body, secrets)
    };
    let cut = scripts.responses_stream(
        "partial",
        "incomplete",
        Some("max_output_tokens"),
        None,
        "m",
    );
    assert_eq!(
        parse(cut, &[]).unwrap().finish_reason,
        LlmFinishReason::Length
    );
    let filtered = scripts.responses_stream("", "incomplete", Some("content_filter"), None, "m");
    assert_eq!(
        parse(filtered, &[]).unwrap().finish_reason,
        LlmFinishReason::ContentFilter
    );
    assert_eq!(
        parse(scripts.responses_failed("boom"), &[]).unwrap_err(),
        LlmError::Transport("boom".to_owned())
    );
    assert_eq!(
        parse(scripts.responses_refusal("no"), &[]).unwrap_err(),
        LlmError::Refused("no".to_owned())
    );
    assert_eq!(
        parse(scripts.responses_truncated_stream(), &[]).unwrap_err(),
        LlmError::Transport("stream closed before response.completed".to_owned())
    );
    let unknown = scripts.responses_stream("x", "incomplete", Some("something_new"), None, "m");
    assert_eq!(
        parse(unknown, &[]).unwrap().finish_reason,
        LlmFinishReason::Other
    );
    assert_eq!(
        parse(
            scripts.responses_error_event("acct_123 said no", "server_error"),
            &["acct_123".to_owned()]
        )
        .unwrap_err(),
        LlmError::Transport("[redacted] said no".to_owned())
    );
}

/// The stream as the backend may vary it: `type` read from the JSON when no
/// `event:` line names it, delta and unknown events skipped, a `reasoning`
/// item skipped even when it carries text, a `[DONE]` sentinel skipped,
/// several message items concatenated in order, the items from the events
/// preferred over the terminal event's `output`, and a missing `usage`
/// counted as one request of zero tokens.
#[test]
fn data_only_unknown_reasoning_and_multi_item_streams() {
    let body = r#"data: {"type":"response.created","response":{"id":"r","status":"in_progress"}}

data: {"type":"response.output_text.delta","delta":"{\"ok"}

data: {"type":"response.output_item.done","item":{"type":"reasoning","id":"rs_1","summary":[],"content":[{"type":"output_text","text":"thinking"}]}}

data: {"type":"response.output_item.done","item":{"type":"message","id":"m1","role":"assistant","content":[{"type":"output_text","text":"{\"ok\":"}]}}

event: some.future.event
data: {"type":"some.future.event","whatever":1}

data: {"type":"response.output_item.done","item":{"type":"function_call","id":"fc_1","name":"tool","arguments":"{}"}}

data: {"type":"response.output_item.done","item":{"type":"message","id":"m2","role":"assistant","content":[{"type":"output_text","text":"true}"},{"type":"output_text","text":""}]}}

data: [DONE]

data: {"type":"response.completed","response":{"id":"r","status":"completed","model":"gpt-x","output":[{"type":"message","id":"m1","role":"assistant","content":[{"type":"output_text","text":"not this"}]}]}}

"#;
    let response = CodexResponsesClient::parse_stream(body.as_bytes(), &[]).unwrap();
    assert_eq!(response.text, "{\"ok\":true}");
    assert_eq!(response.finish_reason, LlmFinishReason::Stop);
    assert_eq!(
        response.usage,
        Some(LlmUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            requests: 1
        })
    );
    assert_eq!(response.model.as_deref(), Some("gpt-x"));
}

#[test]
fn a_terminal_event_without_item_events_uses_its_output() {
    let body = "event: response.completed\ndata: {\"type\":\"response.in_progress\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"reasoning\",\"id\":\"rs\"},{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"from \"},{\"type\":\"refusal\",\"refusal\":\"\"}]},{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"output\"}]}],\"usage\":{\"input_tokens\":3}}}\n\n";
    let response = CodexResponsesClient::parse_stream(body.as_bytes(), &[]).unwrap();
    assert_eq!(response.text, "from output");
    assert_eq!(
        response.usage,
        Some(LlmUsage {
            prompt_tokens: 3,
            completion_tokens: 0,
            requests: 1
        })
    );
    assert_eq!(response.model, None);
    let items = "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"x\"}]}}\n\ndata: {\"type\":\"response.in_progress\",\"response\":{\"status\":\"in_progress\"}}\n\n";
    assert_eq!(
        CodexResponsesClient::parse_stream(items.as_bytes(), &[]).unwrap_err(),
        LlmError::Transport("stream closed before response.completed".to_owned())
    );
}

// Token budget

#[test]
fn german_thousand_words_estimate_between_1200_and_2500_tokens() {
    let text = fixture_text("text/de-1000-words.txt");
    let words = text.split_whitespace().count();
    assert_eq!(words, 1000);
    let tokens = TokenBudget::estimate_tokens(&text, Some(&de()));
    assert!(
        (1200..=2500).contains(&tokens),
        "{tokens} tokens for {words} words"
    );
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let expected = (text.len() as f64 / 3.0).ceil() as i64;
    assert_eq!(tokens, expected);
}

#[test]
fn english_divides_by_more_and_unknown_is_conservative() {
    let text = "The quick brown fox jumps over the lazy dog. ".repeat(20);
    let english = TokenBudget::estimate_tokens(&text, Some(&LanguageTag::from("en-US")));
    let german = TokenBudget::estimate_tokens(&text, Some(&de()));
    let unknown = TokenBudget::estimate_tokens(&text, None);
    assert!(english < german);
    assert_eq!(unknown, german);
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let expected = (text.len() as f64 / 3.6).ceil() as i64;
    assert_eq!(english, expected);
    assert_eq!(TokenBudget::estimate_tokens("", Some(&de())), 0);
    assert_eq!(TokenBudget::estimate_tokens("ä", Some(&de())), 1);
    assert_eq!(
        TokenBudget::bytes_per_token(Some(&LanguageTag::from("EN"))),
        3.6
    );
    assert_eq!(
        TokenBudget::bytes_per_token(Some(&LanguageTag::from("fr"))),
        3.0
    );
}

#[test]
fn input_budget_is_what_remains_and_never_negative() {
    let budget = TokenBudget {
        context_tokens: 8_000,
        reserved_output_tokens: 2_000,
        prompt_overhead_tokens: 1_500,
    };
    assert_eq!(budget.input_budget(), 4_500);
    assert!(budget.fits(4_500));
    assert!(!budget.fits(4_501));
    let tiny = TokenBudget {
        context_tokens: 1_000,
        reserved_output_tokens: 800,
        prompt_overhead_tokens: 500,
    };
    assert_eq!(tiny.input_budget(), 0);
}

#[test]
fn map_notes_ceiling_is_the_chunks_share_within_bounds() {
    let budget = TokenBudget {
        context_tokens: 8_000,
        reserved_output_tokens: 2_000,
        prompt_overhead_tokens: 1_211,
    };
    assert_eq!(budget.input_budget(), 4_789);
    assert_eq!(budget.map_notes_output_tokens(10), 478);
    assert!(budget.fits(10 * budget.map_notes_output_tokens(10)));
    assert_eq!(
        budget.map_notes_output_tokens(1),
        1_500,
        "capped at the ceiling"
    );
    assert_eq!(budget.map_notes_output_tokens(0), 1_500);
    assert_eq!(
        budget.map_notes_output_tokens(100),
        256,
        "never below the floor"
    );
    assert!(!budget.fits(100 * budget.map_notes_output_tokens(100)));
    assert_eq!(
        SummaryPromptBuilder::<chrono::Utc>::max_notes_points(1_500),
        25
    );
    assert_eq!(
        SummaryPromptBuilder::<chrono::Utc>::max_notes_points(478),
        7
    );
    assert_eq!(
        SummaryPromptBuilder::<chrono::Utc>::max_notes_points(100),
        3
    );
}

#[test]
fn endpoint_budgets_follow_the_policy() {
    let endpoint = |context: i64, output: i64| LlmEndpoint {
        context_tokens: context,
        max_output_tokens: output,
        ..LlmEndpoint::new(url::Url::parse("http://127.0.0.1:1234/v1").unwrap(), "m")
    };
    assert_eq!(
        endpoint(32_000, 4_096).cleanup_chunk_budget_tokens(),
        15_488
    );
    assert_eq!(endpoint(8_000, 4_096).cleanup_chunk_budget_tokens(), 3_488);
    assert_eq!(
        endpoint(1_024, 4_096).cleanup_chunk_budget_tokens(),
        256,
        "floor"
    );
    assert_eq!(
        endpoint(32_000, 4_096).summary_reserved_output_tokens(),
        4_096,
        "the ceiling wins"
    );
    assert_eq!(
        endpoint(8_000, 4_096).summary_reserved_output_tokens(),
        2_000,
        "a quarter"
    );
    assert_eq!(
        endpoint(8_000, 1_000).summary_reserved_output_tokens(),
        1_000
    );
    assert_eq!(
        endpoint(1_024, 4_096).summary_reserved_output_tokens(),
        256,
        "floor"
    );
    assert_eq!(BudgetPolicy::MAP_NOTES_CEILING_TOKENS, 1_500);
    assert_eq!(BudgetPolicy::MAP_NOTES_FLOOR_TOKENS, 256);
}

#[test]
fn primary_subtag_drops_region_and_script() {
    assert_eq!(primary_subtag(&LanguageTag::from("de-CH")), "de");
    assert_eq!(primary_subtag(&LanguageTag::from("zh-Hant-TW")), "zh");
    assert_eq!(primary_subtag(&LanguageTag::from("EN")), "en");
}

// Chunker

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

/// The packing invariants over `chunks` of `segments`: order and
/// completeness, sequential indices, the `max_tokens` bound (except for a
/// single oversized segment), the close rule and the leading context.
fn check_packing(
    chunks: &[TranscriptChunk],
    segments: &[TranscriptSegment],
    chunker: &TranscriptChunker,
    language: Option<&LanguageTag>,
    label: &str,
) {
    let flattened: Vec<&TranscriptSegment> =
        chunks.iter().flat_map(|c| c.segments.iter()).collect();
    assert_eq!(flattened.len(), segments.len(), "{label}: concatenation");
    assert!(
        flattened.iter().zip(segments).all(|(a, b)| *a == b),
        "{label}: order"
    );
    assert!(
        chunks.iter().enumerate().all(|(i, c)| c.index == i),
        "{label}: indices"
    );
    assert!(
        chunks.iter().all(|c| !c.segments.is_empty()),
        "{label}: no empty chunk"
    );
    for chunk in chunks {
        assert_eq!(
            chunk.estimated_tokens,
            TranscriptChunker::estimate_segments(&chunk.segments, language),
            "{label}: chunk {} estimate",
            chunk.index
        );
        assert!(
            chunk.estimated_tokens <= chunker.max_tokens || chunk.segments.len() == 1,
            "{label}: chunk {} has {} tokens in {} segments",
            chunk.index,
            chunk.estimated_tokens,
            chunk.segments.len()
        );
        let mut tokens = 0;
        for pair in chunk.segments.windows(2) {
            tokens += TranscriptChunker::estimate_segment(&pair[0], language);
            assert!(
                tokens < chunker.target_tokens || pair[1].speaker_id == pair[0].speaker_id,
                "{label}: chunk {} runs past a speaker turn at {tokens} tokens",
                chunk.index
            );
        }
    }
    assert!(chunks.first().is_none_or(|c| c.leading_context.is_empty()));
    for pair in chunks.windows(2) {
        let (previous, chunk) = (&pair[0], &pair[1]);
        let skip = previous
            .segments
            .len()
            .saturating_sub(chunker.context_segments);
        assert_eq!(
            chunk.leading_context,
            previous.segments[skip..],
            "{label}: context"
        );
        let next = &chunk.segments[0];
        let last = previous.segments.last().unwrap();
        let would_overflow = previous.estimated_tokens
            + TranscriptChunker::estimate_segment(next, language)
            > chunker.max_tokens;
        let turn_after_target = previous.estimated_tokens >= chunker.target_tokens
            && next.speaker_id != last.speaker_id;
        assert!(
            would_overflow || turn_after_target,
            "{label}: chunk {} closed at {} tokens without a reason",
            previous.index,
            previous.estimated_tokens
        );
    }
}

#[test]
fn sixty_minute_fixture_yields_six_to_twelve_chunks_that_concatenate_in_order() {
    let segments = customer_call().segments;
    let chunker = TranscriptChunker::default();
    let chunks = chunker.chunk(&segments, Some(&de()));
    assert!((6..=12).contains(&chunks.len()), "{} chunks", chunks.len());
    check_packing(&chunks, &segments, &chunker, Some(&de()), "fixture");
    for chunk in &chunks[..chunks.len() - 1] {
        assert!(
            chunk.estimated_tokens >= chunker.target_tokens,
            "chunk {} closed early",
            chunk.index
        );
    }
    for pair in chunks.windows(2) {
        assert_eq!(pair[1].leading_context.len(), 3);
    }
    let tokens = TranscriptChunker::estimate_segments(&segments, Some(&de()));
    assert!((12_000..26_000).contains(&tokens), "{tokens}");
}

#[test]
fn an_oversized_segment_gets_its_own_chunk() {
    let a = sample_uuid(410);
    let b = sample_uuid(411);
    let segments = vec![
        segment(0, 10, a),
        segment(1, 10, b),
        segment(2, 2_000, a),
        segment(3, 10, b),
        segment(4, 10, a),
    ];
    let chunker = TranscriptChunker::new(50, 80, 3);
    let chunks = chunker.chunk(&segments, Some(&de()));
    let ids: Vec<Vec<uuid::Uuid>> = chunks
        .iter()
        .map(|c| c.segments.iter().map(|s| s.id).collect())
        .collect();
    assert_eq!(
        ids,
        [
            vec![segments[0].id, segments[1].id],
            vec![segments[2].id],
            vec![segments[3].id, segments[4].id]
        ]
    );
    assert!(chunks[1].estimated_tokens > 80);
    assert_eq!(chunks[1].leading_context, segments[..2]);
    check_packing(&chunks, &segments, &chunker, Some(&de()), "oversized");
}

#[test]
fn empty_input_and_budget_initialiser() {
    assert_eq!(TranscriptChunker::default().chunk(&[], None).len(), 0);
    let clamped = TranscriptChunker::with_budget(500, 3);
    assert_eq!(clamped.target_tokens, 500);
    assert_eq!(clamped.max_tokens, 500);
    let roomy = TranscriptChunker::with_budget(10_000, 3);
    assert_eq!(roomy.target_tokens, 2_000);
    assert_eq!(roomy.max_tokens, 3_000);
    let inverted = TranscriptChunker::new(100, 50, 3);
    assert_eq!(inverted.max_tokens, 100);
}

/// Packing invariants over the fixture under several chunker sizes and
/// both languages, standing in for Swift's seeded generator.
#[test]
fn packing_invariants_hold_for_several_sizes() {
    let call = customer_call();
    let sizes = [(50, 80, 3), (150, 220, 2), (400, 400, 0), (2_000, 3_000, 3)];
    let counts = [1, 2, 7, 60, 300, 900];
    for (target, max, context) in sizes {
        for (offset, count) in counts.iter().enumerate() {
            let language = (offset % 2 == 0).then(|| LanguageTag::from("en"));
            let segments = &call.segments[..*count];
            let chunker = TranscriptChunker::new(target, max, context);
            let chunks = chunker.chunk(segments, language.as_ref());
            assert_ne!(chunks.len(), 0);
            check_packing(
                &chunks,
                segments,
                &chunker,
                language.as_ref(),
                &format!("target {target} max {max} context {context} count {count}"),
            );
        }
    }
    for budget in [64, 300, 1_000, 5_000] {
        let chunker = TranscriptChunker::with_budget(budget, 3);
        let chunks = chunker.chunk(&call.segments[..150], Some(&de()));
        check_packing(
            &chunks,
            &call.segments[..150],
            &chunker,
            Some(&de()),
            "budget",
        );
        assert!(
            chunks
                .iter()
                .all(|c| c.estimated_tokens <= budget || c.segments.len() == 1),
            "budget {budget}"
        );
    }
}

// Strict schema subset

const BANNED: [&str; 15] = [
    "format",
    "pattern",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "minItems",
    "maxItems",
    "anyOf",
    "oneOf",
    "allOf",
    "default",
    "$ref",
    "uniqueItems",
    "patternProperties",
];

/// Every violation of the strict subset in `value`, empty when compliant.
fn schema_problems(value: &Value, path: &str, depth: usize) -> Vec<String> {
    let Value::Object(object) = value else {
        return vec![format!("{path}: schema node is not an object")];
    };
    let mut problems: Vec<String> = object
        .keys()
        .filter(|key| BANNED.contains(&key.as_str()))
        .map(|key| format!("{path}: banned keyword {key}"))
        .collect();
    let kind: Option<String> = match object.get("type") {
        Some(Value::String(name)) => Some(name.clone()),
        Some(Value::Array(names)) => {
            let strings: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
            if strings.len() != 2 || !strings.contains(&"null") {
                problems.push(format!("{path}: type array must be [type, null]"));
            }
            strings
                .iter()
                .find(|s| **s != "null")
                .map(|s| (*s).to_owned())
        }
        _ => {
            problems.push(format!("{path}: missing type"));
            None
        }
    };
    if depth > 5 && matches!(kind.as_deref(), Some("object" | "array")) {
        problems.push(format!("{path}: nesting depth {depth} exceeds 5"));
    }
    match kind.as_deref() {
        Some("object") => {
            if object.get("additionalProperties") != Some(&Value::Bool(false)) {
                problems.push(format!("{path}: additionalProperties must be false"));
            }
            let Some(Value::Object(properties)) = object.get("properties") else {
                problems.push(format!("{path}: object without properties"));
                return problems;
            };
            let Some(Value::Array(required)) = object.get("required") else {
                problems.push(format!("{path}: object without required"));
                return problems;
            };
            let required: std::collections::BTreeSet<&str> =
                required.iter().filter_map(Value::as_str).collect();
            let names: std::collections::BTreeSet<&str> =
                properties.keys().map(String::as_str).collect();
            if required != names {
                problems.push(format!("{path}: every property must be required"));
            }
            for (name, child) in properties {
                problems.extend(schema_problems(child, &format!("{path}.{name}"), depth + 1));
            }
        }
        Some("array") => match object.get("items") {
            Some(items) => problems.extend(schema_problems(items, &format!("{path}[]"), depth + 1)),
            None => problems.push(format!("{path}: array without items")),
        },
        Some("string" | "integer" | "number" | "boolean") => {}
        other => problems.push(format!("{path}: unexpected type {other:?}")),
    }
    problems
}

fn sample_schema() -> JsonSchema {
    JsonSchema::object(vec![
        (
            "title",
            JsonSchema::string().described("under 80 characters"),
        ),
        ("count", JsonSchema::integer()),
        ("score", JsonSchema::number().nullable()),
        ("flag", JsonSchema::boolean()),
        ("kind", JsonSchema::string_enum(&["a", "b"])),
        (
            "items",
            JsonSchema::array(JsonSchema::object(vec![
                ("id", JsonSchema::string()),
                ("note", JsonSchema::string().nullable()),
                ("tags", JsonSchema::array(JsonSchema::string())),
            ])),
        ),
    ])
    .described("sample")
}

#[test]
fn builder_emits_the_strict_subset() {
    let json = sample_schema().json_value();
    assert_eq!(schema_problems(&json, "root", 1), Vec::<String>::new());
    assert_eq!(json["type"], "object");
    assert_eq!(json["description"], "sample");
    assert_eq!(json["additionalProperties"], false);
    assert_eq!(
        json["properties"]["score"]["type"],
        json!(["number", "null"])
    );
    assert_eq!(json["properties"]["kind"]["enum"], json!(["a", "b"]));
    assert_eq!(
        json["properties"]["items"]["items"]["required"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        sample_schema().property_names(),
        ["title", "count", "score", "flag", "kind", "items"]
    );
}

#[test]
fn the_probe_schema_is_strict_too() {
    assert_eq!(
        schema_problems(
            &OpenAiCompatibleClient::probe_schema().json_value(),
            "root",
            1
        ),
        Vec::<String>::new()
    );
    let steno_core::LlmResponseFormat::JsonSchema {
        name,
        schema,
        strict,
    } = OpenAiCompatibleClient::probe_request().response_format
    else {
        panic!("the probe asks for a JSON schema");
    };
    assert_eq!(name, "probe");
    assert!(strict);
    assert_eq!(schema, OpenAiCompatibleClient::probe_schema().json_value());
    assert_eq!(schema["properties"]["ok"]["type"], "boolean");
}

#[test]
fn walker_rejects_loose_schemas() {
    let loose = json!({
        "type": "object", "properties": {"a": {"type": "string", "format": "date"}},
        "required": [], "additionalProperties": true,
    });
    let problems = schema_problems(&loose, "root", 1);
    assert!(problems.contains(&"root: additionalProperties must be false".to_owned()));
    assert!(problems.contains(&"root: every property must be required".to_owned()));
    assert!(problems.contains(&"root.a: banned keyword format".to_owned()));
    let mut deep = json!({"type": "string"});
    for _ in 0..6 {
        deep = json!({"type": "object", "properties": {"x": deep}, "required": ["x"], "additionalProperties": false});
    }
    assert!(
        schema_problems(&deep, "root", 1)
            .iter()
            .any(|p| p.contains("exceeds 5"))
    );
}

#[test]
fn prompt_text_is_compact_and_ordered() {
    let expected = "{\n  \"title\": string,  // under 80 characters\n  \"count\": integer,\n  \"score\": number | null,\n  \"flag\": boolean,\n  \"kind\": \"a\" | \"b\",\n  \"items\": [{\n    \"id\": string,\n    \"note\": string | null,\n    \"tags\": [string]\n  }]\n}";
    assert_eq!(sample_schema().prompt_text(), expected);
    assert_eq!(
        JsonSchema::object(vec![
            ("lead", JsonSchema::string()),
            ("text", JsonSchema::string())
        ])
        .prompt_text(),
        "{ \"lead\": string, \"text\": string }"
    );
}

#[test]
fn draft_and_notes_schemas_are_strict_and_use_the_templates_section_ids() {
    let template = steno_core::SummaryTemplate::bundled_with_id("interview").unwrap();
    let builder = SummaryPromptBuilder::new(template.clone(), chrono::Utc);
    let ids: Vec<Value> = template.sections.iter().map(|s| json!(s.id)).collect();
    assert_eq!(
        builder.draft_schema().json_value()["properties"]["sections"]["items"]["properties"]["id"]
            ["enum"],
        Value::Array(ids)
    );
    assert_eq!(
        schema_problems(&builder.draft_schema().json_value(), "root", 1),
        Vec::<String>::new()
    );
    assert_eq!(
        schema_problems(&builder.notes_schema().json_value(), "root", 1),
        Vec::<String>::new()
    );
    assert!(
        builder.draft_schema().json_value()["properties"]
            .get("language")
            .is_none()
    );
    assert!(
        !builder
            .draft_schema()
            .prompt_text()
            .contains("\"language\"")
    );
    assert_eq!(
        builder.draft_schema().json_value()["properties"]["tasks"]["items"]["properties"]["priority"]
            ["enum"],
        json!(["low", "normal", "high"])
    );
}

// Stub server

/// The builder every `with_http` caller should start from leaves the TLS
/// provider installed, which reqwest's `rustls-no-provider` build does not
/// do on its own.
#[test]
fn the_http_client_builder_installs_a_tls_provider() {
    let _client = steno_llm::transport::http_client_builder().build().unwrap();
    assert!(rustls::crypto::CryptoProvider::get_default().is_some());
}

#[tokio::test]
async fn stub_accepts_a_post_returns_the_script_and_records_the_parsed_request() {
    let server = StubChatServer::start().await.unwrap();
    server.enqueue([scripts.completion(
        "hello",
        Some("stop"),
        Some(LlmUsage {
            prompt_tokens: 3,
            completion_tokens: 1,
            requests: 1,
        }),
        "stub-model",
    )]);
    let body = ChatCompletionRequest {
        model: "stub-model".to_owned(),
        messages: vec![ChatMessage {
            role: "user".to_owned(),
            content: "hi".to_owned(),
        }],
        temperature: Some(0.0),
        max_tokens: Some(16),
        max_completion_tokens: None,
        response_format: Some(ChatResponseFormat::json_object()),
    };
    let http = steno_llm::transport::default_http_client();
    let response = http
        .post(server.base_url().join("/v1/chat/completions").unwrap())
        .header("Authorization", "Bearer sk-test")
        .header("X-Steno-Purpose", "cleanup")
        .header("Content-Type", "application/json")
        .body(wire::encode(&body).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let decoded: ChatCompletionResponse = wire::decode(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(decoded.choices[0].message.content.as_deref(), Some("hello"));
    assert_eq!(decoded.choices[0].finish_reason.as_deref(), Some("stop"));
    assert_eq!(decoded.usage.unwrap().prompt_tokens, Some(3));

    server.received(1).await;
    let recorded = &server.requests()[0];
    assert_eq!(recorded.method, "POST");
    assert_eq!(recorded.path, "/v1/chat/completions");
    assert_eq!(recorded.authorization(), Some("Bearer sk-test"));
    assert_eq!(recorded.purpose.as_deref(), Some("cleanup"));
    assert_eq!(recorded.chat.as_ref(), Some(&body));
    assert!(recorded.responses.is_none());
    assert_eq!(recorded.in_flight_on_arrival, 1);
    assert_eq!(server.max_in_flight(), 1);
}

#[tokio::test]
async fn stub_answers_unscripted_requests_with_404_and_consults_the_responder() {
    let server = StubChatServer::start().await.unwrap();
    let http = steno_llm::transport::default_http_client();
    let url = server.base_url().join("/v1/models").unwrap();
    let first = http.get(url.clone()).send().await.unwrap();
    assert_eq!(first.status().as_u16(), 404);
    server.respond(scripts.format_rejecting_server(&["a", "b"], &[], scripts.text("x")));
    let second = http.get(url).send().await.unwrap();
    assert_eq!(second.status().as_u16(), 200);
    let list: ModelList = wire::decode(&second.bytes().await.unwrap()).unwrap();
    let ids: Vec<&str> = list.data.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(server.request_count(), 2);
}

#[tokio::test]
async fn stub_held_responses_show_up_as_in_flight_until_released() {
    let server = StubChatServer::start().await.unwrap();
    server.enqueue([scripts.text("1"), scripts.text("2")]);
    server.hold_responses();
    let url = server.base_url().join("/v1/chat/completions").unwrap();
    let status = |url: url::Url| async move {
        steno_llm::transport::default_http_client()
            .post(url)
            .body("{}")
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    };
    let first = tokio::spawn(status(url.clone()));
    let second = tokio::spawn(status(url));
    server.received(2).await;
    assert_eq!(server.in_flight(), 2);
    server.release();
    assert_eq!(first.await.unwrap(), 200);
    assert_eq!(second.await.unwrap(), 200);
    assert_eq!(server.max_in_flight(), 2);
    assert_eq!(server.in_flight(), 0);
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
    let json = String::from_utf8(wire::encode(&request).unwrap()).unwrap();
    assert!(json.contains("\"max_tokens\":100"), "{json}");
    assert!(
        json.contains("\"response_format\":{\"json_schema\":{\"name\":\"n\""),
        "{json}"
    );
    assert!(!json.contains("max_completion_tokens"));
    assert_eq!(
        wire::decode::<ChatCompletionRequest>(json.as_bytes()).unwrap(),
        request
    );

    let parts = "{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"a\"},{\"type\":\"text\",\"text\":\"b\"}]},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}";
    let response: ChatCompletionResponse = wire::decode(parts.as_bytes()).unwrap();
    assert_eq!(response.choices[0].message.content.as_deref(), Some("ab"));
    assert_eq!(response.usage.unwrap().completion_tokens, Some(2));

    let error: ChatErrorEnvelope =
        wire::decode(b"{\"error\":{\"message\":\"m\",\"code\":400}}").unwrap();
    assert_eq!(error.error.code, Some(json!(400)));
}

// Fixtures

#[test]
fn the_fixtures_are_what_the_swift_generators_produced() {
    let standup = standup();
    assert_eq!(standup.segments.len(), 24);
    let labels: Vec<&str> = standup
        .speakers
        .iter()
        .map(|s| s.cluster_label.as_str())
        .collect();
    assert_eq!(labels, ["Speaker 1", "Speaker 2", "Speaker 3"]);
    assert!(standup.segments.iter().all(|s| s.text == s.raw_text));
    assert!(standup.segments.iter().any(|s| s.text.contains("git hub")));
    assert!(
        standup
            .segments
            .iter()
            .any(|s| s.text.contains("kuber netes"))
    );
    assert_eq!(standup.meeting.language, Some(de()));
    assert_eq!(standup.meeting.template_id, "daily-standup");
    let names: Vec<&str> = standup
        .participants
        .iter()
        .map(|p| p.display_name.as_str())
        .collect();
    assert_eq!(names, ["Mara", "Jérôme", "Nicolai"]);
    for pair in standup.segments.windows(2) {
        assert_eq!(pair[1].start, pair[0].end);
    }

    let call = customer_call();
    assert_eq!(call.segments.len(), 900);
    assert_eq!(call.segments.last().unwrap().end, 3_600.0 - 0.25);
    assert_eq!(call.meeting.duration, 3_600.0);
    let labels: Vec<&str> = call
        .speakers
        .iter()
        .map(|s| s.cluster_label.as_str())
        .collect();
    assert_eq!(labels, ["Me", "Speaker 1", "Speaker 2"]);
    let ids: std::collections::HashSet<uuid::Uuid> = call.segments.iter().map(|s| s.id).collect();
    assert_eq!(ids.len(), 900, "unique segment ids");
    let me = call.speakers[0].id;
    assert!(
        call.segments
            .iter()
            .filter(|s| s.lane == AudioLane::Mic)
            .all(|s| s.speaker_id == Some(me))
    );
    assert!(
        call.segments
            .iter()
            .filter(|s| s.lane == AudioLane::System)
            .all(|s| s.speaker_id != Some(me))
    );

    let cleanup = steno_llm::inputs::cleanup_input(&standup);
    assert_eq!(cleanup.segments, standup.segments);
    assert_eq!(cleanup.language, Some(de()));
    assert_eq!(cleanup.known_people, standup.persons);
    let summary = steno_llm::inputs::summary_input(&standup, None);
    assert_eq!(summary.template.id, "daily-standup");
    assert_eq!(summary.meeting, standup.meeting);
    let other = steno_llm::inputs::summary_input(
        &standup,
        steno_core::SummaryTemplate::bundled_with_id("interview"),
    );
    assert_eq!(other.template.id, "interview");
}

/// The mode is spelled as Swift's raw values in JSON and in text, and
/// the chain order is the declaration order.
#[test]
fn structured_output_mode_keeps_the_swift_spelling() {
    let modes = [
        (StructuredOutputMode::JsonSchema, "jsonSchema"),
        (StructuredOutputMode::JsonObject, "jsonObject"),
        (StructuredOutputMode::PromptOnly, "promptOnly"),
    ];
    for (mode, text) in modes {
        assert_eq!(serde_json::to_string(&mode).unwrap(), format!("\"{text}\""));
        assert_eq!(
            serde_json::from_str::<StructuredOutputMode>(&format!("\"{text}\"")).unwrap(),
            mode
        );
        assert_eq!(mode.as_str(), text);
        assert_eq!(text.parse::<StructuredOutputMode>().unwrap(), mode);
    }
    assert!(serde_json::from_str::<StructuredOutputMode>("\"json_schema\"").is_err());
    assert_eq!(
        StructuredOutputMode::ALL,
        [
            StructuredOutputMode::JsonSchema,
            StructuredOutputMode::JsonObject,
            StructuredOutputMode::PromptOnly
        ]
    );
}
