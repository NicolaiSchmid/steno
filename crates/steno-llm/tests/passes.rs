//! The cleanup and summary passes over the stub server, their
//! post-processing, and the golden prompts (a diff in a golden is a
//! reviewed prompt change).

mod common;

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use common::*;
use steno_core::{
    AudioLane, LanguageTag, LlmUsage, MeetingSummarizer, SummaryBullet, SummaryInput,
    SummaryTemplate, TaskPriority, TranscriptCleaner, TranscriptSegment,
};
use steno_llm::cleanup::{CleanupDraft, CleanupDraftSegment, glossary};
use steno_llm::inputs::{cleanup_input, summary_input};
use steno_llm::labels::SpeakerLabels;
use steno_llm::summary::{DraftBullet, DraftSection, NotesTopic};
use steno_llm::testing::{Responder, StubChatServer, StubResponse, parse_segments, scripts};
use steno_llm::{
    AnalysisDraft, ChunkNotes, CleanupPromptBuilder, DraftSpeakerName, DraftTask, LlmEndpoint,
    LlmError, LlmMeetingSummarizer, LlmTranscriptCleaner, OpenAiCompatibleClient, RetryPolicy,
    SummaryPromptBuilder, TranscriptChunk, TranscriptChunker,
};

fn usage(prompt: i64, completion: i64) -> LlmUsage {
    LlmUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        requests: 1,
    }
}

fn client(
    server: &StubChatServer,
    context_tokens: i64,
) -> (Arc<OpenAiCompatibleClient>, LlmEndpoint) {
    let endpoint = LlmEndpoint {
        context_tokens,
        ..LlmEndpoint::new(server.base_url().clone(), "stub-model")
    };
    let client = OpenAiCompatibleClient::new(endpoint.clone(), None).with_retry(RetryPolicy::NONE);
    (Arc::new(client), endpoint)
}

/// A cleaner whose small chunks split the standup into several.
fn cleaner(server: &StubChatServer) -> LlmTranscriptCleaner {
    let (client, endpoint) = client(server, 32_000);
    LlmTranscriptCleaner::new(client, endpoint).with_chunker(TranscriptChunker::new(150, 220, 3))
}

fn summarizer(server: &StubChatServer, context_tokens: i64) -> LlmMeetingSummarizer<Utc> {
    let (client, endpoint) = client(server, context_tokens);
    LlmMeetingSummarizer::new(client, endpoint, Utc)
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

/// The chunk's segments as the request lists them, each through `edit`.
fn answering(
    edit: impl Fn(&str, Vec<(i64, String)>) -> Vec<(i64, String)> + Send + Sync + 'static,
) -> Responder {
    Arc::new(move |request| {
        let first_user = &request
            .chat
            .as_ref()?
            .messages
            .iter()
            .find(|m| m.role == "user")?
            .content;
        let purpose = request.purpose.clone().unwrap_or_default();
        let draft = CleanupDraft {
            segments: edit(&purpose, parse_segments(first_user))
                .into_iter()
                .map(|(index, text)| CleanupDraftSegment { index, text })
                .collect(),
        };
        Some(scripts.json(&draft, Some(usage(100, 50))))
    })
}

fn purposes(server: &StubChatServer) -> Vec<String> {
    server
        .requests()
        .iter()
        .map(|r| r.purpose.clone().unwrap_or_default())
        .collect()
}

#[tokio::test]
async fn cleanup_rewrites_the_text_and_keeps_count_order_ids_and_raw_text() {
    let server = StubChatServer::start().await.unwrap();
    server.respond(scripts.cleanup_echo(usage(100, 50), |_, text| {
        Some(capitalised(&text.replace("git hub", "GitHub")))
    }));
    let input = cleanup_input(&standup());
    let output = cleaner(&server).clean(&input).await.unwrap();
    assert_eq!(output.segments.len(), input.segments.len());
    for (out, inp) in output.segments.iter().zip(&input.segments) {
        assert_eq!(
            (out.id, &out.raw_text, out.speaker_id, out.start),
            (inp.id, &inp.raw_text, inp.speaker_id, inp.start)
        );
        assert!(out.text.starts_with(char::is_uppercase), "{}", out.text);
    }
    assert!(output.segments[2].text.contains("GitHub"));
    assert_eq!(output.failed_chunks.len(), 0);
    let count = i64::try_from(server.request_count()).unwrap();
    assert!(count >= 2, "several chunks");
    assert_eq!(
        output.usage,
        LlmUsage {
            prompt_tokens: 100 * count,
            completion_tokens: 50 * count,
            requests: count
        }
    );
    let system = &server.requests()[0].chat.clone().unwrap().messages[0].content;
    assert!(system.contains("Names to spell exactly like this: Mara, Jérôme, Nicolai."));
    assert!(system.contains("Meeting language: German."));

    // Participants first, then known people, each once.
    assert_eq!(
        glossary(&cleanup_input(&customer_call())),
        ["Nicolai", "Petra Vogel", "Tom Berger", "Jérôme"]
    );
    let mut empty = input.clone();
    empty.segments.clear();
    let before = server.request_count();
    let output = cleaner(&server).clean(&empty).await.unwrap();
    assert_eq!((output.segments.len(), output.usage), (0, LlmUsage::ZERO));
    assert_eq!(server.request_count(), before, "no request for no segments");
}

/// The speakerphone call: a model that answered `Me: text` for every
/// segment had its label stored as transcript. Labels are stripped before
/// validation, with no retry.
#[tokio::test]
async fn a_model_echoing_the_speaker_label_is_cleaned_without_a_retry() {
    let server = StubChatServer::start().await.unwrap();
    server.respond(scripts.cleanup_echo(usage(100, 50), |index, text| {
        Some(if index % 2 == 0 {
            format!("Me: {text}")
        } else {
            format!("[{index}] Speaker 1: {text}")
        })
    }));
    let input = cleanup_input(&customer_call());
    let chunker = TranscriptChunker::new(800, 1_200, 3);
    let chunks = chunker.chunk(&input.segments, input.language.as_ref());
    let (client, endpoint) = client(&server, 32_000);
    let output = LlmTranscriptCleaner::new(client, endpoint)
        .with_chunker(chunker)
        .clean(&input)
        .await
        .unwrap();
    assert_eq!(output.failed_chunks.len(), 0);
    let texts: Vec<&str> = output.segments.iter().map(|s| s.text.as_str()).collect();
    let originals: Vec<&str> = input.segments.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, originals);
    assert_eq!(server.request_count(), chunks.len(), "no chunk was retried");

    let mut speakers = standup().speakers;
    let mut me = speakers[0].clone();
    me.id = sample_uuid(300);
    me.cluster_label = "Me".to_owned();
    speakers.push(me);
    let labels = SpeakerLabels::new(&speakers);
    let cases = [
        ("Me: Danke dir.", "Danke dir."),
        ("me:Danke dir.", "Danke dir."),
        ("[3] Speaker 1: Hallo.", "Hallo."),
        ("[3] Hallo.", "Hallo."),
        ("Unknown speaker: Hallo.", "Hallo."),
        ("  Speaker 2 : Hallo.", "Hallo."),
        ("**Me:** Danke dir.", "Danke dir."),
        ("_Speaker 1_: Danke dir.", "Danke dir."),
        ("(Me) Danke dir.", "Danke dir."),
        ("Me - Danke dir.", "Danke dir."),
        ("\u{A0}Speaker 1\u{3000}: Hallo.", "Hallo."),
        ("Me:", ""),
        // No label of this meeting, or no label at all, stays.
        ("Speaker 10: Hallo.", "Speaker 10: Hallo."),
        ("Meeting: agenda", "Meeting: agenda"),
        (
            "Me-too products are everywhere.",
            "Me-too products are everywhere.",
        ),
        ("Me, I think so: yes", "Me, I think so: yes"),
        (
            "Das ist, was ich meine, und zwar wirklich so: alles.",
            "Das ist, was ich meine, und zwar wirklich so: alles.",
        ),
        (" Hallo.", " Hallo."),
        // A colon with a combining mark is no separator.
        ("Speaker 1:\u{301} Hallo.", "Speaker 1:\u{301} Hallo."),
        ("Speaker 1\n: Hallo.", "Speaker 1\n: Hallo."),
        (" \u{301}[2] Hallo.", "Hallo."),
        ("Speaker 1:  \u{301}Hallo.", " \u{301}Hallo."),
    ];
    for (text, stripped) in cases {
        assert_eq!(
            CleanupDraft::stripping_label(text, &labels),
            stripped,
            "{text:?}"
        );
    }
    // The colon must sit within the first 32 grapheme clusters.
    let last_in_stretch = format!("{}Speaker 1: Hallo.", "* ".repeat(11));
    assert_eq!(
        CleanupDraft::stripping_label(&last_in_stretch, &labels),
        "Hallo."
    );
    let late = format!("{}Speaker 1: Hallo.", "*".repeat(23));
    assert_eq!(CleanupDraft::stripping_label(&late, &labels), late);
}

fn one_segment(words: usize) -> TranscriptChunk {
    let text = vec!["wort"; words].join(" ");
    TranscriptChunk {
        index: 0,
        segments: vec![TranscriptSegment {
            id: sample_uuid(500),
            meeting_id: sample_uuid(1),
            start: 0.0,
            end: 1.0,
            speaker_id: None,
            lane: AudioLane::Mixed,
            text: text.clone(),
            raw_text: text,
        }],
        leading_context: Vec::new(),
        estimated_tokens: 1,
    }
}

/// An answer keeps the chunk's count, its indices once each, and roughly
/// its word count: within 0.7 to 1.3, or one word off ("Git Hub").
#[test]
fn a_cleanup_answer_must_fit_its_chunk() {
    let reworded = |from, to| {
        format!(
            "Segment 0 changed from {from} to {to} words; keep the wording, only fix spelling, casing and punctuation."
        )
    };
    let ratio_cases = [
        (10, 7, None),
        (10, 13, None),
        (10, 6, Some(reworded(10, 6))),
        (2, 1, None),
        (1, 2, None),
        (3, 1, Some(reworded(3, 1))),
        (5, 0, Some("Segment 0 came back empty.".to_owned())),
        (0, 4, None),
    ];
    for (original, cleaned, problem) in ratio_cases {
        let draft = CleanupDraft {
            segments: vec![CleanupDraftSegment {
                index: 0,
                text: vec!["wort"; cleaned].join(" "),
            }],
        };
        assert_eq!(
            draft.problems(&one_segment(original)),
            Vec::from_iter(problem),
            "{original} -> {cleaned}"
        );
    }

    let chunk = TranscriptChunker::default().chunk(&standup().segments, Some(&de()))[0].clone();
    let mut shuffled = CleanupDraft::echo(&chunk);
    shuffled.segments.reverse();
    assert_eq!(shuffled.problems(&chunk), Vec::<String>::new(), "any order");
    let texts: Vec<String> = chunk.segments.iter().map(|s| s.text.clone()).collect();
    assert_eq!(shuffled.ordered_texts(), texts);
    // Two segments merged into one index is caught even at the right count.
    let mut duplicated = CleanupDraft::echo(&chunk);
    duplicated.segments[1].index = 0;
    assert!(duplicated.problems(&chunk)[0].starts_with("Indices must be 0 to"));
}

/// A chunk whose answer does not fit, does not decode or is refused gets
/// one retry with the answer and the reason, then stays raw.
#[tokio::test]
async fn a_bad_cleanup_answer_is_retried_once_then_kept_raw() {
    let wrong_count = || {
        let mut draft = CleanupDraft::echo(
            &TranscriptChunker::default().chunk(&standup().segments, Some(&de()))[0],
        );
        draft.segments.pop();
        scripts.json(&draft, None)
    };
    let cases: Vec<(&str, StubResponse, &str)> = vec![
        (
            "wrong count",
            wrong_count(),
            "Expected 24 segments, got 23.",
        ),
        (
            "not JSON",
            scripts.text("not json at all"),
            "the model returned invalid JSON: malformed JSON",
        ),
        (
            "truncated",
            scripts.truncated("{\"segments\": ["),
            "the model's answer was cut off",
        ),
        (
            "no segments",
            scripts.text("{}"),
            "the model returned invalid JSON: missing key segments at root",
        ),
        (
            "refused",
            scripts.refusal("I will not edit this."),
            "the model refused: I will not edit this.",
        ),
    ];
    let standup = standup();
    for (what, reply, reason) in cases {
        let server = StubChatServer::start().await.unwrap();
        server.enqueue([reply.clone(), reply]);
        let (client, endpoint) = client(&server, 32_000);
        let output = LlmTranscriptCleaner::new(client, endpoint)
            .clean(&cleanup_input(&standup))
            .await
            .unwrap();
        assert_eq!(output.failed_chunks, [0], "{what}");
        assert_eq!(output.segments, standup.segments, "{what}");
        assert_eq!(
            output.usage.requests, 2,
            "{what}: a refusal still costs a request"
        );
        assert_eq!(purposes(&server), ["cleanup", "cleanup-retry"], "{what}");
        let retry = server.requests().pop().unwrap().chat.unwrap();
        assert_eq!(retry.messages[2].role, "assistant", "{what}");
        assert!(
            retry.messages[3].content.contains(reason),
            "{what}: {}",
            retry.messages[3].content
        );
    }
}

/// Of several chunks, a failing one is listed and kept raw while the rest
/// is cleaned; a retry that comes back right is applied.
#[tokio::test]
async fn only_the_chunks_that_keep_failing_stay_raw() {
    let input = cleanup_input(&standup());
    let chunks =
        TranscriptChunker::new(150, 220, 3).chunk(&input.segments, input.language.as_ref());
    assert!(chunks.len() >= 3);
    let first_of = |chunk: &TranscriptChunk| chunk.segments[0].text.clone();
    let (failing, recovering) = (first_of(&chunks[0]), first_of(&chunks[1]));
    let server = StubChatServer::start().await.unwrap();
    server.respond(answering(move |purpose, segments| {
        let first = segments[0].1.clone();
        let keeps_failing = first == failing;
        let fails_once = first == recovering && purpose == "cleanup";
        segments
            .into_iter()
            .map(|(index, text)| {
                (
                    index,
                    if keeps_failing || fails_once {
                        "x".to_owned()
                    } else {
                        capitalised(&text)
                    },
                )
            })
            .collect()
    }));
    let output = cleaner(&server).clean(&input).await.unwrap();
    assert_eq!(output.failed_chunks, [0]);
    let mut offset = 0;
    for chunk in &chunks {
        let cleaned = &output.segments[offset..offset + chunk.segments.len()];
        for (out, inp) in cleaned.iter().zip(&chunk.segments) {
            let expected = if chunk.index == 0 {
                inp.text.clone()
            } else {
                capitalised(&inp.text)
            };
            assert_eq!(out.text, expected, "chunk {}", chunk.index);
        }
        offset += chunk.segments.len();
    }
    assert_eq!(server.request_count(), chunks.len() + 2);
    assert_eq!(
        output.usage.requests,
        i64::try_from(chunks.len() + 2).unwrap()
    );
}

#[tokio::test]
async fn cleanup_keeps_to_the_concurrency_limit_and_propagates_transport_failures() {
    let server = StubChatServer::start().await.unwrap();
    server.respond(scripts.cleanup_echo(usage(1, 1), |_, text| Some(text.to_owned())));
    server.hold_responses();
    let input = cleanup_input(&customer_call());
    let chunk_count = TranscriptChunker::default()
        .chunk(&input.segments, input.language.as_ref())
        .len();
    let (client, endpoint) = client(&server, 32_000);
    let pass = Arc::new(
        LlmTranscriptCleaner::new(client, endpoint).with_chunker(TranscriptChunker::default()),
    );
    let task = tokio::spawn({
        let (pass, input) = (Arc::clone(&pass), input.clone());
        async move { pass.clean(&input).await }
    });
    server.received(2).await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        server.request_count(),
        2,
        "the third chunk waits for a free slot"
    );
    server.release();
    assert_eq!(task.await.unwrap().unwrap().failed_chunks.len(), 0);
    assert_eq!(server.request_count(), chunk_count);
    assert_eq!(server.max_in_flight(), 2);

    // A failure of the service, not of the answer, fails the pass.
    let server = StubChatServer::start().await.unwrap();
    server.respond(Arc::new(|_| Some(scripts.unauthorized())));
    let error = cleaner(&server).clean(&input).await.unwrap_err();
    assert!(matches!(
        downcast::<LlmError>(&error),
        LlmError::Http { status: 401, .. }
    ));
}

fn default_input() -> SummaryInput {
    summary_input(&standup(), SummaryTemplate::bundled_with_id("default"))
}

fn midnight(year: i32, month: u32, day: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap()
}

// One canned answer, every post-processing rule checked against it.
#[tokio::test]
async fn single_shot_builds_the_summary_output_from_the_draft() {
    let server = StubChatServer::start().await.unwrap();
    server.enqueue([scripts.completion(
        &canned("summary-default-standup"),
        Some("stop"),
        Some(usage(1_500, 400)),
        "stub-model",
    )]);
    let input = default_input();
    let output = summarizer(&server, 32_000).summarize(&input).await.unwrap();
    let chat = server.requests()[0].chat.clone().unwrap();
    assert_eq!(purposes(&server), ["summary"]);
    assert_eq!(
        (chat.temperature, chat.max_tokens),
        (Some(0.2), Some(4_096))
    );
    assert_eq!(
        chat.response_format.unwrap().json_schema.unwrap().name,
        "meeting_analysis"
    );
    assert_eq!(output.usage, usage(1_500, 400));
    assert_eq!(
        output.title,
        "Daily Standup: Onboarding, CI und Obsidian-Export"
    );
    assert_eq!(
        (output.language.clone(), output.summary.language.clone()),
        (Some(de()), Some(de()))
    );
    let ids: Vec<&str> = output
        .summary
        .sections
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["executive-summary", "full-summary"],
        "template order, empty optional dropped"
    );
    assert_eq!(output.decisions.len(), 2, "duplicate decisions collapse");
    assert_eq!(output.tasks.len(), 4, "the blank task is dropped");
    let [first, second, third, fourth] = &output.tasks[..] else {
        panic!("four tasks")
    };
    assert_eq!(
        first.assignee_name.as_deref(),
        Some("Speaker 2"),
        "an unconfirmed speaker keeps its label"
    );
    assert_eq!(
        (first.assignee_person_id, first.priority, first.due_date),
        (None, TaskPriority::High, None)
    );
    let jerome = Some(sample_uuid(PERSON_JEROME));
    assert_eq!(
        (second.assignee_name.as_deref(), second.assignee_person_id),
        (Some("Jérôme"), jerome)
    );
    assert_eq!(
        second.due_date,
        Some(input.meeting.started_at - chrono::TimeDelta::hours(9)),
        "midnight UTC"
    );
    assert_eq!(
        (third.assignee_person_id, third.due_date),
        (jerome, Some(midnight(2026, 9, 30)))
    );
    assert_eq!(
        (fourth.assignee_person_id, fourth.priority),
        (Some(sample_uuid(PERSON_MARA)), TaskPriority::Low)
    );
    assert!(
        output
            .tasks
            .iter()
            .all(|t| t.meeting_id == input.meeting.id && !t.done)
    );
    assert_eq!(
        first.id,
        steno_core::derived_uuid(input.meeting.id, "task-0")
    );
    let [suggestion] = &output.speaker_names[..] else {
        panic!("one suggestion")
    };
    assert_eq!(suggestion.speaker_id, sample_uuid(STANDUP_SPEAKER_TWO));
    assert_eq!(
        (suggestion.name.as_deref(), suggestion.confidence),
        (Some("Nicolai"), 0.9)
    );
}

/// An answer that does not decode gets one repair round naming the path;
/// a truncated one fails at once. Usage counts every call.
#[tokio::test]
async fn a_bad_summary_answer_is_repaired_once() {
    let good = canned("summary-default-standup");
    let urgent = good.replace("\"priority\": \"high\"", "\"priority\": \"urgent\"");
    assert_ne!(urgent, good);
    let cases: Vec<(&str, Vec<StubResponse>, Result<&str, LlmError>)> = vec![
        (
            "malformed",
            vec![
                scripts.completion(
                    "{\"title\": \"x\", \"sections\": [}",
                    Some("stop"),
                    Some(usage(700, 3)),
                    "m",
                ),
                scripts.completion(&good, Some("stop"), None, "m"),
            ],
            Ok("Validation error: malformed JSON"),
        ),
        (
            "unknown priority",
            vec![scripts.text(&urgent), scripts.text(&good)],
            Ok("tasks.[0].priority"),
        ),
        (
            "still broken",
            vec![scripts.text("nope"), scripts.text("still nope")],
            Err(LlmError::InvalidJson(String::new())),
        ),
        (
            "truncated",
            vec![scripts.truncated("{\"title\": \"cut")],
            Err(LlmError::Truncated),
        ),
    ];
    for (what, replies, expected) in cases {
        let server = StubChatServer::start().await.unwrap();
        let calls = replies.len();
        server.enqueue(replies);
        let outcome = summarizer(&server, 32_000)
            .summarize(&default_input())
            .await;
        assert_eq!(server.request_count(), calls, "{what}");
        match expected {
            Ok(reason) => {
                let output = outcome.unwrap();
                assert_eq!(purposes(&server), ["summary", "summary-repair"]);
                let repair = server.requests().pop().unwrap().chat.unwrap();
                assert!(repair.messages[1].content.contains(reason), "{what}");
                assert_eq!(
                    repair.response_format.unwrap().json_schema.unwrap().name,
                    "meeting_analysis"
                );
                assert_eq!(output.tasks[0].priority, TaskPriority::High);
                assert_eq!(
                    output.usage.requests, 2,
                    "{what}: a body without usage still counts"
                );
            }
            Err(LlmError::InvalidJson(_)) => {
                assert!(
                    matches!(
                        downcast::<LlmError>(&outcome.unwrap_err()),
                        LlmError::InvalidJson(_)
                    ),
                    "{what}"
                );
            }
            Err(error) => assert_eq!(downcast::<LlmError>(&outcome.unwrap_err()), error, "{what}"),
        }
    }
}

fn section(id: &str, heading: &str, bullets: &[(&str, &str)]) -> DraftSection {
    DraftSection {
        id: id.to_owned(),
        heading: heading.to_owned(),
        bullets: bullets
            .iter()
            .map(|(lead, text)| DraftBullet {
                lead: (*lead).to_owned(),
                text: (*text).to_owned(),
            })
            .collect(),
    }
}

fn draft(
    sections: Vec<DraftSection>,
    tasks: Vec<DraftTask>,
    speaker_names: Vec<DraftSpeakerName>,
) -> AnalysisDraft {
    AnalysisDraft {
        title: "T".to_owned(),
        sections,
        decisions: Vec::new(),
        tasks,
        speaker_names,
    }
}

fn task(text: &str, assignee: Option<&str>, due: Option<&str>) -> DraftTask {
    DraftTask {
        text: text.to_owned(),
        assignee: assignee.map(str::to_owned),
        priority: TaskPriority::Normal,
        due_date: due.map(str::to_owned),
    }
}

fn speaker_name(label: &str, name: Option<&str>, confidence: f64) -> DraftSpeakerName {
    DraftSpeakerName {
        speaker_label: label.to_owned(),
        name: name.map(str::to_owned),
        confidence,
        evidence: format!(" {label} "),
    }
}

/// Sections come out in template order: unknown ids dropped, a section
/// split in two merged, empty optional ones dropped, required ones kept
/// even when empty, blank headings and a blank title replaced.
#[test]
fn summary_sections_follow_the_template() {
    let input = default_input();
    let mut answer = draft(
        vec![
            section("open-questions", " ", &[("Zeitplan:", " Oktober? ")]),
            section("full-summary", "", &[]),
            section("executive-summary", "", &[("A", "a")]),
            section("not-a-section", "Nope", &[("x", "y")]),
            section("executive-summary", "Kurz", &[("C", "c")]),
        ],
        Vec::new(),
        Vec::new(),
    );
    answer.title = "  ".to_owned();
    answer.decisions = vec![" a ".to_owned(), "A".to_owned(), String::new()];
    let output = answer.summary_output(&input, LlmUsage::ZERO, 0.3);
    assert_eq!(output.title, input.meeting.title);
    let sections: Vec<(&str, &str, usize)> = output
        .summary
        .sections
        .iter()
        .map(|s| (s.id.as_str(), s.heading.as_str(), s.bullets.len()))
        .collect();
    assert_eq!(
        sections,
        [
            ("executive-summary", "Kurz", 2),
            ("open-questions", "Open Questions", 1)
        ]
    );
    assert_eq!(
        output.summary.sections[1].bullets,
        [SummaryBullet {
            lead: "Zeitplan".to_owned(),
            text: "Oktober?".to_owned()
        }]
    );
    assert_eq!(output.decisions, ["a"]);
    assert!(!output.summary.plain_text().contains("Nope"));

    for template in SummaryTemplate::bundled() {
        let input = summary_input(&standup(), Some(template));
        let output =
            draft(Vec::new(), Vec::new(), Vec::new()).summary_output(&input, LlmUsage::ZERO, 0.3);
        let required: Vec<(&str, &str)> = template
            .sections
            .iter()
            .filter(|s| s.required)
            .map(|s| (s.id.as_str(), s.heading.as_str()))
            .collect();
        let found: Vec<(&str, &str)> = output
            .summary
            .sections
            .iter()
            .map(|s| (s.id.as_str(), s.heading.as_str()))
            .collect();
        assert_eq!(found, required, "{}", template.id);
        // The draft schema offers exactly the template's ids.
        let ids: Vec<serde_json::Value> = template
            .sections
            .iter()
            .map(|s| serde_json::json!(s.id))
            .collect();
        let schema = SummaryPromptBuilder::new(template.clone(), Utc)
            .draft_schema()
            .json_value();
        assert_eq!(
            schema["properties"]["sections"]["items"]["properties"]["id"]["enum"],
            serde_json::Value::Array(ids)
        );
    }
}

/// Assignees resolve by full name, unique first name, a speaker label's
/// person, then a known person; due dates are strict `YYYY-MM-DD`.
#[test]
fn tasks_get_resolved_assignees_and_strict_due_dates() {
    let input = summary_input(&customer_call(), None);
    let labels = SpeakerLabels::new(&input.speakers);
    let nicolai = Some(sample_uuid(PERSON_NICOLAI));
    let resolve = |name| AnalysisDraft::resolve_assignee(name, &input, &labels);
    for (name, expected) in [
        ("petra vogel", "Petra Vogel"),
        ("Petra", "Petra Vogel"),
        ("Tom", "Tom Berger"),
        ("Somebody Else", "Somebody Else"),
    ] {
        assert_eq!(resolve(Some(name)).unwrap().name, expected, "{name}");
    }
    for (name, expected, person) in [
        ("Nicolai", "Nicolai", nicolai),
        ("me", "Nicolai", nicolai),
        ("Speaker 1", "Speaker 1", None),
        ("Jérôme", "Jérôme", Some(sample_uuid(PERSON_JEROME))),
        ("Somebody Else", "Somebody Else", None),
    ] {
        let resolved = resolve(Some(name)).unwrap();
        assert_eq!(
            (resolved.name.as_str(), resolved.person_id),
            (expected, person),
            "{name}"
        );
    }
    assert!(resolve(Some("  ")).is_none() && resolve(None).is_none());
    for (raw, due) in [
        (Some("2026-09-26"), Some(midnight(2026, 9, 26))),
        (Some(" 2026-09-26 "), Some(midnight(2026, 9, 26))),
        (Some("next Friday"), None),
        (Some("2026-9-26"), None),
        (Some("26.09.2026"), None),
        (Some("2026-02-30"), None),
        (None, None),
    ] {
        assert_eq!(AnalysisDraft::parse_due_date(raw), due, "{raw:?}");
    }
    let answer = draft(
        Vec::new(),
        vec![task("Doku", Some("Speaker 3"), Some("Freitag"))],
        Vec::new(),
    );
    let output = answer.summary_output(&default_input(), LlmUsage::ZERO, 0.3);
    assert_eq!(
        output.tasks[0].assignee_name.as_deref(),
        Some("Jérôme"),
        "a confirmed speaker is its person"
    );
    assert_eq!(output.tasks[0].due_date, None);
}

/// Known labels only, the strongest per speaker, clamped to 1, at or above
/// the minimum confidence, in speaker order; never a rename.
#[test]
fn speaker_name_suggestions_are_filtered() {
    let speakers = standup().speakers;
    let labels = SpeakerLabels::new(&speakers);
    let drafts = [
        speaker_name("speaker 3", Some("Jérôme"), 1.7),
        speaker_name("Speaker 2", Some("Nico"), 0.4),
        speaker_name("Speaker 2", Some("Nicolai"), 0.8),
        speaker_name("Speaker 1", Some("Mara"), 0.29),
        speaker_name("Speaker 1", Some(""), 0.9),
        speaker_name("Speaker 1", None, 0.9),
        speaker_name("Speaker 7", Some("Ghost"), 0.9),
    ];
    let found = |minimum| -> Vec<(uuid::Uuid, String, f64)> {
        AnalysisDraft::suggestions(&drafts, &labels, &speakers, minimum)
            .into_iter()
            .map(|s| (s.speaker_id, s.name.unwrap(), s.confidence))
            .collect()
    };
    let two = sample_uuid(STANDUP_SPEAKER_TWO);
    let three = sample_uuid(STANDUP_SPEAKER_THREE);
    assert_eq!(
        found(0.3),
        [
            (two, "Nicolai".to_owned(), 0.8),
            (three, "Jérôme".to_owned(), 1.0)
        ]
    );
    assert_eq!(found(0.9), [(three, "Jérôme".to_owned(), 1.0)]);
    let input = default_input();
    let answer = draft(
        vec![section(
            "executive-summary",
            "E",
            &[("L", "Speaker 2 sagt etwas.")],
        )],
        Vec::new(),
        vec![speaker_name("Speaker 2", Some("Nicolai"), 1.0)],
    );
    let output = answer.summary_output(&input, LlmUsage::ZERO, 0.3);
    assert_eq!(
        output.summary.sections[0].bullets[0].text,
        "Speaker 2 sagt etwas."
    );
    assert_eq!(output.speaker_names[0].evidence, "Speaker 2", "trimmed");
}

#[test]
fn the_meeting_language_and_time_zone_drive_prompts_and_output() {
    for (language, name, rendered) in [
        (Some(de()), "German", "de"),
        (None, "English", "en"),
        (Some(LanguageTag::from("de-CH")), "German", "de-CH"),
    ] {
        let mut export = standup();
        export.meeting.language = language.clone();
        let input = summary_input(&export, SummaryTemplate::bundled_with_id("default"));
        let single =
            SummaryPromptBuilder::new(input.template.clone(), Utc).build_single_shot(&input);
        let system = &single.messages[0].content;
        assert!(
            system.contains(&format!("Output language: {name}.")),
            "{name}"
        );
        let output =
            draft(Vec::new(), Vec::new(), Vec::new()).summary_output(&input, LlmUsage::ZERO, 0.3);
        assert_eq!(
            output.summary.language,
            Some(LanguageTag::from(rendered)),
            "the renderer always gets one"
        );
        assert_eq!(
            output.language, language,
            "an untagged meeting is never stored as English"
        );
    }
    // The fixture starts at 09:00 UTC on Thursday 2026-09-24.
    let input = default_input();
    let date_line = |zone: chrono_tz::Tz| {
        let system = SummaryPromptBuilder::new(input.template.clone(), zone)
            .build_single_shot(&input)
            .messages[0]
            .content
            .clone();
        system
            .lines()
            .find(|l| l.starts_with("Meeting date: "))
            .unwrap()
            .to_owned()
    };
    assert!(
        date_line(chrono_tz::Europe::Berlin).starts_with("Meeting date: 2026-09-24 (Thursday).")
    );
    assert!(
        date_line(chrono_tz::Pacific::Honolulu)
            .starts_with("Meeting date: 2026-09-23 (Wednesday).")
    );
}

fn call_input() -> SummaryInput {
    summary_input(
        &customer_call(),
        SummaryTemplate::bundled_with_id("default"),
    )
}

fn notes(points: usize, point_words: usize) -> ChunkNotes {
    ChunkNotes {
        chunk_index: 0,
        topics: vec![NotesTopic {
            topic: "Thema".to_owned(),
            points: vec!["wort ".repeat(point_words); points],
        }],
        decisions: Vec::new(),
        task_candidates: Vec::new(),
        speaker_cues: Vec::new(),
    }
}

/// Map answers by `map`, the canned draft for the reduce.
fn map_reduce_server(
    map: impl Fn(&steno_llm::testing::RecordedRequest) -> StubResponse + Send + Sync + 'static,
) -> Responder {
    let draft = canned("summary-default-standup");
    Arc::new(move |request| {
        Some(match request.purpose.as_deref() {
            Some("summary-map" | "summary-map-repair") => map(request),
            _ => scripts.completion(&draft, Some("stop"), Some(usage(2_000, 400)), "m"),
        })
    })
}

/// At 8k the hour-long call takes map calls, one per chunk, each with its
/// share of the input budget as ceiling, then one reduce over all notes.
#[tokio::test]
async fn a_long_transcript_is_mapped_then_reduced() {
    let server = StubChatServer::start().await.unwrap();
    let notes_text = canned("notes-customer-call");
    server.respond(map_reduce_server(move |_| {
        scripts.completion(&notes_text, Some("stop"), Some(usage(1_000, 100)), "m")
    }));
    let summarizer = summarizer(&server, 8_000);
    let output = summarizer.summarize(&call_input()).await.unwrap();
    let calls = purposes(&server);
    let maps = calls.iter().filter(|p| *p == "summary-map").count();
    assert!((6..=12).contains(&maps), "{maps} map calls");
    assert_eq!(calls.last().map(String::as_str), Some("summary-reduce"));
    assert_eq!(calls.len(), maps + 1);
    assert!(server.max_in_flight() <= 2);
    let budget = summarizer.budget_for(&call_input(), None);
    let ceiling = budget.map_notes_output_tokens(maps);
    assert!(ceiling * i64::try_from(maps).unwrap() <= budget.input_budget());
    let requests = server.requests();
    for (part, map) in requests[..maps].iter().enumerate() {
        let chat = map.chat.as_ref().unwrap();
        assert_eq!(chat.max_tokens, Some(ceiling));
        assert_eq!(
            chat.response_format
                .as_ref()
                .unwrap()
                .json_schema
                .as_ref()
                .unwrap()
                .name,
            "chunk_notes"
        );
        assert!(requests[..maps].iter().any(|m| {
            m.body_text()
                .contains(&format!("part {} of {maps}", part + 1))
        }));
    }
    let reduce = requests[maps].chat.clone().unwrap();
    assert_eq!(reduce.max_tokens, Some(2_000), "a quarter of 8k");
    for index in 0..maps {
        assert!(
            reduce.messages[1]
                .content
                .contains(&format!("\"chunkIndex\" : {index}")),
            "chunk {index}"
        );
    }
    assert_eq!(output.usage.requests, i64::try_from(maps + 1).unwrap());
    assert_eq!(output.summary.sections[0].id, "executive-summary");

    // At 32k one call suffices.
    let server = StubChatServer::start().await.unwrap();
    server.respond(map_reduce_server(|_| scripts.text("{}")));
    summarize_with(&server, 32_000).await.unwrap();
    assert_eq!(purposes(&server), ["summary"]);
}

async fn summarize_with(
    server: &StubChatServer,
    context: i64,
) -> steno_core::BoundaryResult<steno_core::SummaryOutput> {
    summarizer(server, context).summarize(&call_input()).await
}

/// A map answer that does not decode is repaired once with the notes
/// schema; the chunk index is Steno's, not the model's.
#[tokio::test]
async fn a_bad_map_answer_is_repaired_once_and_its_chunk_index_overruled() {
    let server = StubChatServer::start().await.unwrap();
    server.respond(map_reduce_server(|request| {
        if request.purpose.as_deref() == Some("summary-map")
            && request.body_text().contains("part 3 of")
        {
            scripts.text("Here are my notes, no JSON today.")
        } else {
            scripts.json(&notes(2, 12), Some(usage(500, 50)))
        }
    }));
    summarize_with(&server, 8_000).await.unwrap();
    let purposes = purposes(&server);
    let maps = purposes.iter().filter(|p| *p == "summary-map").count();
    assert_eq!(
        purposes
            .iter()
            .filter(|p| *p == "summary-map-repair")
            .count(),
        1
    );
    let repair = server
        .requests()
        .into_iter()
        .find(|r| r.purpose.as_deref() == Some("summary-map-repair"))
        .unwrap()
        .chat
        .unwrap();
    assert_eq!(
        repair.response_format.unwrap().json_schema.unwrap().name,
        "chunk_notes"
    );
    assert!(
        repair.messages[1]
            .content
            .contains("Here are my notes, no JSON today.")
    );
    let reduce = &server.requests().pop().unwrap().chat.unwrap().messages[1].content;
    for index in 0..maps {
        assert!(
            reduce.contains(&format!("\"chunkIndex\" : {index}")),
            "chunk {index}"
        );
    }
}

/// Every refusal to fit comes before a call that cannot succeed: the
/// transcript at 4k before any call, oversized notes before the reduce;
/// a truncated map answer fails the pass.
#[tokio::test]
async fn a_summary_that_cannot_fit_fails_before_the_call() {
    let server = StubChatServer::start().await.unwrap();
    server.respond(map_reduce_server(|_| scripts.text("{}")));
    let error = summarize_with(&server, 4_000).await.unwrap_err();
    assert!(
        matches!(
            downcast::<LlmError>(&error),
            LlmError::TranscriptTooLong { .. }
        ),
        "{error}"
    );
    assert_eq!(server.request_count(), 0, "nothing was sent");

    let server = StubChatServer::start().await.unwrap();
    server.respond(map_reduce_server(|_| {
        scripts.json(&notes(30, 120), Some(usage(10, 5)))
    }));
    let error = summarize_with(&server, 8_000).await.unwrap_err();
    let LlmError::TranscriptTooLong {
        estimated_tokens,
        budget,
    } = downcast::<LlmError>(&error)
    else {
        panic!("expected TranscriptTooLong, got {error}");
    };
    assert!(estimated_tokens > budget);
    assert!(
        purposes(&server).iter().all(|p| p == "summary-map"),
        "no reduce call"
    );

    let server = StubChatServer::start().await.unwrap();
    server.respond(map_reduce_server(|_| {
        scripts.truncated("{\"chunkIndex\": 0, \"topics\": [")
    }));
    let error = summarize_with(&server, 8_000).await.unwrap_err();
    assert_eq!(downcast::<LlmError>(&error), LlmError::Truncated);
    assert!(
        server.request_count() <= 2,
        "the first truncated chunk cancels the rest"
    );
}

#[test]
fn every_prompt_matches_its_golden() {
    let standup = standup();
    let input = cleanup_input(&standup);
    for (tag, golden) in [
        ("de", "prompts/cleanup-de.txt"),
        ("en", "prompts/cleanup-en.txt"),
    ] {
        let language = LanguageTag::from(tag);
        let chunks = TranscriptChunker::new(150, 220, 3).chunk(&input.segments, Some(&language));
        let request = CleanupPromptBuilder::default().build(
            &chunks[1],
            Some(&language),
            &glossary(&input),
            &SpeakerLabels::new(&input.speakers),
        );
        assert_golden(&render(&request), golden);
    }
    for id in SummaryTemplate::BUNDLED_IDS {
        let input = summary_input(&standup, SummaryTemplate::bundled_with_id(id));
        let builder = SummaryPromptBuilder::new(input.template.clone(), Utc);
        assert_golden(
            &render(&builder.build_single_shot(&input)),
            &format!("prompts/summary-single-{id}.txt"),
        );
        assert_golden(
            &format!("{}\n", builder.template_blocks()),
            &format!("prompts/template-blocks-{id}.txt"),
        );
    }
    let mut untagged = standup.clone();
    untagged.meeting.language = None;
    let input = summary_input(&untagged, SummaryTemplate::bundled_with_id("default"));
    let request = SummaryPromptBuilder::new(input.template.clone(), Utc).build_single_shot(&input);
    assert_golden(&render(&request), "prompts/summary-single-default-en.txt");

    let input = default_input();
    let builder = SummaryPromptBuilder::new(input.template.clone(), Utc);
    let chunks = TranscriptChunker::new(150, 220, 3).chunk(&input.segments, Some(&de()));
    let map = builder.build_map(&input, &chunks[1], chunks.len(), 1_500);
    assert_golden(&render(&map), "prompts/summary-map-default.txt");
    let notes = [
        ChunkNotes {
            chunk_index: 0,
            topics: vec![NotesTopic {
                topic: "Onboarding".to_owned(),
                points: vec![
                    "Speaker 2 hat den Pull Request für das Onboarding gemerged.".to_owned(),
                ],
            }],
            decisions: Vec::new(),
            task_candidates: Vec::new(),
            speaker_cues: vec![DraftSpeakerName {
                speaker_label: "Speaker 2".to_owned(),
                name: Some("Nicolai".to_owned()),
                confidence: 0.5,
                evidence: "nicolai, willst du starten?".to_owned(),
            }],
        },
        ChunkNotes {
            chunk_index: 1,
            topics: vec![NotesTopic {
                topic: "Obsidian-Export".to_owned(),
                points: vec!["Die Firma Müller will den Export bis Freitag.".to_owned()],
            }],
            decisions: vec!["Der Export wird bis Freitag umgesetzt.".to_owned()],
            task_candidates: vec![DraftTask {
                text: "Export nach Obsidian umsetzen.".to_owned(),
                assignee: Some("Speaker 2".to_owned()),
                priority: TaskPriority::High,
                due_date: Some("2026-09-25".to_owned()),
            }],
            speaker_cues: Vec::new(),
        },
    ];
    assert_golden(
        &render(&builder.build_reduce(&input, &notes)),
        "prompts/summary-reduce-default.txt",
    );
    let repair = SummaryPromptBuilder::<Utc>::build_repair(
        &builder.build_single_shot(&input),
        &builder.draft_schema(),
        "{\"title\": ",
        "malformed JSON at root",
    );
    assert_golden(&render(&repair), "prompts/summary-repair.txt");
}
