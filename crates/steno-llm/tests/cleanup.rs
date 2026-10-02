//! The cleanup pass over the stub server: count, order, ids and raw text
//! preserved; validation of merged, reworded and emptied answers; one retry
//! then raw; bounded concurrency; transport failures propagate.
//! Swift: `CleanupTests`, `CleanupValidationTests`.

mod common;

use std::sync::Arc;

use common::*;
use steno_core::{AudioLane, LanguageTag, LlmUsage, TranscriptCleaner, TranscriptSegment};
use steno_llm::cleanup::{CleanupDraft, CleanupDraftSegment, glossary};
use steno_llm::inputs::cleanup_input;
use steno_llm::testing::{StubChatServer, default_usage, parse_segments, scripts};
use steno_llm::{
    CleanupPromptBuilder, LlmEndpoint, LlmError, LlmTranscriptCleaner, OpenAiCompatibleClient,
    RetryPolicy, TranscriptChunk, TranscriptChunker,
};

fn de() -> LanguageTag {
    LanguageTag::from("de")
}

/// A cleaner over the stub server with no retries and the system clock;
/// every request is answered at once, so nothing ever sleeps.
fn cleaner(
    server: &StubChatServer,
    chunker: Option<TranscriptChunker>,
    configure: impl FnOnce(&mut LlmEndpoint),
) -> LlmTranscriptCleaner {
    let mut endpoint = LlmEndpoint::new(server.base_url().clone(), "stub-model");
    configure(&mut endpoint);
    let client = OpenAiCompatibleClient::new(endpoint.clone(), None).with_retry(RetryPolicy::NONE);
    let cleaner = LlmTranscriptCleaner::new(Arc::new(client), endpoint);
    match chunker {
        Some(chunker) => cleaner.with_chunker(chunker),
        None => cleaner,
    }
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A "perfect" model: capitalises the first letter and fixes two known STT
/// errors, keeping the word count.
fn fixing(text: &str) -> String {
    capitalised(
        &text
            .replace("git hub", "GitHub")
            .replace("jerome", "Jérôme"),
    )
}

fn echo(chunk: &TranscriptChunk) -> CleanupDraft {
    CleanupDraft::echo(chunk)
}

fn words(count: usize) -> String {
    vec!["wort"; count].join(" ")
}

fn starts_uppercase(text: &str) -> bool {
    text.chars().next().is_some_and(char::is_uppercase)
}

#[tokio::test]
async fn preserves_count_order_ids_and_raw_text_and_rewrites_text() {
    let server = StubChatServer::start().await.unwrap();
    server.respond(scripts.cleanup_echo(
        LlmUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            requests: 1,
        },
        |_, text| Some(fixing(text)),
    ));
    let input = cleanup_input(&standup());
    let output = cleaner(&server, Some(TranscriptChunker::new(150, 220, 3)), |_| {})
        .clean(&input)
        .await
        .unwrap();

    assert_eq!(output.segments.len(), input.segments.len());
    for (out, inp) in output.segments.iter().zip(&input.segments) {
        assert_eq!(out.id, inp.id);
        assert_eq!(out.raw_text, inp.raw_text);
        assert_eq!(out.speaker_id, inp.speaker_id);
        assert_eq!(out.start, inp.start);
    }
    assert_eq!(output.failed_chunks.len(), 0);
    assert!(output.segments[2].text.contains("GitHub"));
    assert!(output.segments[4].text.contains("Jérôme"));
    assert!(output.segments.iter().all(|s| starts_uppercase(&s.text)));
    let requests = server.requests();
    assert!(
        requests.len() >= 2,
        "the small chunker splits 24 segments into several chunks"
    );
    assert!(
        requests
            .iter()
            .all(|r| r.purpose.as_deref() == Some("cleanup"))
    );
    let count = i64::try_from(requests.len()).unwrap();
    assert_eq!(
        output.usage,
        LlmUsage {
            prompt_tokens: 100 * count,
            completion_tokens: 50 * count,
            requests: count
        }
    );
    let chat = requests[0].chat.as_ref().unwrap();
    let system = &chat.messages[0].content;
    assert!(system.contains("Names to spell exactly like this: Mara, Jérôme, Nicolai."));
    assert!(system.contains("Meeting language: German."));
    assert_eq!(chat.temperature, Some(0.0));
    assert_eq!(
        chat.response_format
            .as_ref()
            .and_then(|f| f.json_schema.as_ref())
            .map(|s| s.name.as_str()),
        Some("transcript_cleanup")
    );
}

#[tokio::test]
async fn a_wrong_count_chunk_is_retried_once_then_kept_raw() {
    let server = StubChatServer::start().await.unwrap();
    let chunker = TranscriptChunker::new(150, 220, 3);
    let input = cleanup_input(&standup());
    let chunks = chunker.chunk(&input.segments, input.language.as_ref());
    assert!(chunks.len() >= 3);
    let victim_chunk = chunks[1].clone();
    let victim = victim_chunk.segments[0].text.clone();
    // Drops the last segment of the victim chunk's answer, both times.
    server.respond(Arc::new(move |request| {
        let first_user = request
            .chat
            .as_ref()?
            .messages
            .iter()
            .find(|m| m.role == "user")?
            .content
            .clone();
        let mut segments = parse_segments(&first_user);
        if segments.first().is_some_and(|(_, text)| *text == victim) {
            segments.pop();
        }
        let draft = CleanupDraft {
            segments: segments
                .into_iter()
                .map(|(index, text)| CleanupDraftSegment { index, text })
                .collect(),
        };
        Some(scripts.json(&draft, Some(default_usage())))
    }));
    let output = cleaner(&server, Some(chunker), |_| {})
        .clean(&input)
        .await
        .unwrap();

    assert_eq!(output.failed_chunks, [victim_chunk.index]);
    let texts: Vec<&str> = output.segments.iter().map(|s| s.text.as_str()).collect();
    let inputs: Vec<&str> = input.segments.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        texts, inputs,
        "the echo changes nothing, the failed chunk stays raw"
    );
    let requests = server.requests();
    let retries: Vec<_> = requests
        .iter()
        .filter(|r| r.purpose.as_deref() == Some("cleanup-retry"))
        .collect();
    assert_eq!(retries.len(), 1);
    assert_eq!(requests.len(), chunks.len() + 1);
    let retry = retries[0].chat.as_ref().unwrap();
    assert_eq!(retry.messages.len(), 4);
    assert_eq!(retry.messages[2].role, "assistant");
    assert!(retry.messages[3].content.contains(&format!(
        "Expected {} segments, got {}.",
        victim_chunk.segments.len(),
        victim_chunk.segments.len() - 1
    )));
    assert_eq!(
        output.usage.requests,
        i64::try_from(chunks.len() + 1).unwrap()
    );
}

#[test]
fn reworded_segments_are_rejected_but_one_word_differences_pass() {
    let standup = standup();
    let chunk = TranscriptChunker::default().chunk(&standup.segments, Some(&de()))[0].clone();
    let mut draft = echo(&chunk);
    assert_eq!(draft.problems(&chunk).len(), 0);
    let texts: Vec<String> = chunk.segments.iter().map(|s| s.text.clone()).collect();
    assert_eq!(draft.ordered_texts(), texts);

    draft.segments[2].text = "Heute schaue ich mir die flaky Tests in der CI-Pipeline an, die laufen seit dem GitHub-Upgrade nicht mehr stabil.".to_owned();
    assert_eq!(draft.problems(&chunk).len(), 0);
    assert!(draft.ordered_texts()[2].starts_with("Heute"));

    draft.segments[3].text = "Blocker?".to_owned();
    assert!(
        draft.problems(&chunk)[0].starts_with("Segment 3 changed from 4 to 1 words"),
        "{:?}",
        draft.problems(&chunk)
    );

    draft.segments[3].text = "hast du einen blocker?".to_owned();
    draft.segments[5].text = String::new();
    assert_eq!(draft.problems(&chunk), ["Segment 5 came back empty."]);

    draft.segments[5].text = chunk.segments[5].text.clone();
    draft.segments[0].index = 7;
    assert!(draft.problems(&chunk)[0].starts_with("Indices must be 0 to"));

    draft.segments[0].index = 0;
    let in_order = draft.ordered_texts();
    draft.segments.swap(0, 1);
    assert_eq!(draft.problems(&chunk).len(), 0);
    assert_eq!(
        draft.ordered_texts(),
        in_order,
        "answers arrive in any order"
    );
}

#[tokio::test]
async fn never_exceeds_max_concurrent_requests() {
    let server = StubChatServer::start().await.unwrap();
    server.respond(scripts.cleanup_echo(
        LlmUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            requests: 1,
        },
        |_, text| Some(text.to_owned()),
    ));
    server.hold_responses();
    let input = cleanup_input(&customer_call());
    let chunker = TranscriptChunker::default();
    let chunk_count = chunker
        .chunk(&input.segments, input.language.as_ref())
        .len();
    assert!(chunk_count >= 6);
    let cleaner = Arc::new(cleaner(&server, Some(chunker), |e| {
        e.max_concurrent_requests = 2;
    }));
    let task = tokio::spawn({
        let cleaner = Arc::clone(&cleaner);
        let input = input.clone();
        async move { cleaner.clean(&input).await }
    });
    server.received(2).await;
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        server.request_count(),
        2,
        "the third chunk waits for a free slot"
    );
    assert_eq!(server.in_flight(), 2);
    server.release();
    let output = task.await.unwrap().unwrap();
    assert_eq!(output.failed_chunks.len(), 0);
    assert_eq!(server.request_count(), chunk_count);
    assert_eq!(server.max_in_flight(), 2);
    assert_eq!(output.usage.requests, i64::try_from(chunk_count).unwrap());
}

#[tokio::test]
async fn transport_failures_propagate_instead_of_falling_back_to_raw() {
    let server = StubChatServer::start().await.unwrap();
    // Every chunk in flight gets the same answer, so the first error to
    // surface is always this one.
    server.respond(Arc::new(|_| Some(scripts.unauthorized())));
    let cleaner = cleaner(&server, Some(TranscriptChunker::new(150, 220, 3)), |_| {});
    let error = cleaner.clean(&cleanup_input(&standup())).await.unwrap_err();
    assert_eq!(
        downcast::<LlmError>(&error),
        LlmError::Http {
            status: 401,
            body: "Incorrect API key provided".to_owned()
        }
    );
}

#[tokio::test]
async fn undecodable_and_truncated_answers_fall_back_to_raw_after_one_retry() {
    let server = StubChatServer::start().await.unwrap();
    server.enqueue([
        scripts.text("not json at all"),
        scripts.truncated("{\"segments\": ["),
    ]);
    let standup = standup();
    let chunker = TranscriptChunker::default();
    let output = cleaner(&server, Some(chunker), |_| {})
        .clean(&cleanup_input(&standup))
        .await
        .unwrap();
    assert_eq!(chunker.chunk(&standup.segments, Some(&de())).len(), 1);
    assert_eq!(output.failed_chunks, [0]);
    assert_eq!(output.segments, standup.segments);
    assert_eq!(server.request_count(), 2);
    let last = server.requests().pop().unwrap().chat.unwrap();
    assert!(
        last.messages
            .last()
            .unwrap()
            .content
            .contains("invalid JSON")
    );
}

#[tokio::test]
async fn empty_transcript_makes_no_request() {
    let server = StubChatServer::start().await.unwrap();
    let mut input = cleanup_input(&standup());
    input.segments.clear();
    let output = cleaner(&server, None, |_| {}).clean(&input).await.unwrap();
    assert_eq!(output.segments.len(), 0);
    assert_eq!(output.usage, LlmUsage::ZERO);
    assert_eq!(server.requests().len(), 0);
}

#[test]
fn glossary_deduplicates_and_orders_participants_first() {
    let mut input = cleanup_input(&standup());
    let person = |number: u32, name: &str| steno_core::Person {
        id: sample_uuid(number),
        display_name: name.to_owned(),
        email: None,
        embedding: None,
        sample_count: 0,
        created_at: codex_now(),
    };
    input.known_people.push(person(99, "nicolai"));
    input.known_people.push(person(98, "  "));
    assert_eq!(glossary(&input), ["Mara", "Jérôme", "Nicolai"]);
    assert_eq!(
        glossary(&cleanup_input(&customer_call())),
        ["Nicolai", "Petra Vogel", "Tom Berger", "Jérôme"]
    );
}

#[test]
fn output_tokens_scale_with_the_chunk_and_stay_under_the_ceiling() {
    let builder = CleanupPromptBuilder::new(1_000);
    let call = customer_call();
    let chunks = TranscriptChunker::default().chunk(&call.segments, Some(&de()));
    assert_eq!(builder.output_tokens(&chunks[0], Some(&de())), 1_000);
    let small =
        TranscriptChunker::new(50, 80, 3).chunk(&standup().segments, Some(&de()))[0].clone();
    let tokens = builder.output_tokens(&small, Some(&de()));
    assert!((256..1_000).contains(&tokens), "{tokens}");
}

// Validation from the model's side

fn first_chunk() -> TranscriptChunk {
    TranscriptChunker::default().chunk(&standup().segments, Some(&de()))[0].clone()
}

#[test]
fn merged_answers_are_rejected_by_count_and_by_duplicate_index() {
    let chunk = first_chunk();
    let count = chunk.segments.len();
    let mut merged = echo(&chunk);
    let second = merged.segments[1].text.clone();
    merged.segments[0].text.push(' ');
    merged.segments[0].text.push_str(&second);
    merged.segments.remove(1);
    let remaining: Vec<String> = std::iter::once(0)
        .chain(2..count)
        .map(|i| i.to_string())
        .collect();
    assert_eq!(
        merged.problems(&chunk),
        [
            format!("Expected {count} segments, got {}.", count - 1),
            format!(
                "Indices must be 0 to {}, each exactly once; got {}.",
                count - 1,
                remaining.join(", ")
            ),
        ]
    );
    // Renumbering the merged answer hides nothing: the count still fails.
    for (index, segment) in merged.segments.iter_mut().enumerate() {
        segment.index = i64::try_from(index).unwrap();
    }
    assert_eq!(
        merged.problems(&chunk)[0],
        format!("Expected {count} segments, got {}.", count - 1)
    );
    assert_eq!(merged.problems(&chunk).len(), 2);

    let mut duplicated = echo(&chunk);
    duplicated.segments[1].index = 0;
    let problems = duplicated.problems(&chunk);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].starts_with(&format!(
        "Indices must be 0 to {}, each exactly once;",
        count - 1
    )));

    let mut added = echo(&chunk);
    added.segments.push(CleanupDraftSegment {
        index: i64::try_from(count).unwrap(),
        text: "extra".to_owned(),
    });
    assert_eq!(
        added.problems(&chunk).len(),
        2,
        "count and indices both fail"
    );

    let mut negative = echo(&chunk);
    negative.segments[0].index = -1;
    assert!(negative.problems(&chunk)[0].starts_with("Indices must be"));
}

#[test]
fn reordered_answers_are_accepted_and_read_back_in_segment_order() {
    let chunk = first_chunk();
    let mut shuffled = echo(&chunk);
    shuffled.segments.reverse();
    assert_eq!(shuffled.problems(&chunk).len(), 0);
    let texts: Vec<String> = chunk.segments.iter().map(|s| s.text.clone()).collect();
    assert_eq!(shuffled.ordered_texts(), texts);
}

#[test]
fn word_ratio_boundaries_are_exact() {
    let chunk = |word_count: usize| {
        let text = words(word_count);
        TranscriptChunk {
            index: 0,
            segments: vec![TranscriptSegment {
                id: sample_uuid(500),
                meeting_id: sample_uuid(1),
                start: 0.0,
                end: 1.0,
                speaker_id: Some(sample_uuid(110)),
                lane: AudioLane::Mixed,
                text: text.clone(),
                raw_text: text,
            }],
            leading_context: Vec::new(),
            estimated_tokens: 1,
        }
    };
    let problems = |original: usize, cleaned: usize| {
        CleanupDraft {
            segments: vec![CleanupDraftSegment {
                index: 0,
                text: words(cleaned),
            }],
        }
        .problems(&chunk(original))
    };
    let reworded = |from: usize, to: usize| {
        format!(
            "Segment 0 changed from {from} to {to} words; keep the wording, only fix spelling, casing and punctuation."
        )
    };
    // Ratio 0.7 and 1.3 are inside the range; one word past them is outside.
    assert_eq!(problems(10, 7).len(), 0);
    assert_eq!(problems(10, 13).len(), 0);
    assert_eq!(problems(10, 6), [reworded(10, 6)]);
    assert_eq!(problems(10, 14).len(), 1);
    // A difference of one word passes whatever the ratio says ("Git Hub").
    assert_eq!(problems(2, 1).len(), 0);
    assert_eq!(problems(1, 2).len(), 0);
    assert_eq!(problems(3, 1), [reworded(3, 1)]);
    // Emptied is its own message; an empty original accepts anything.
    assert_eq!(problems(5, 0), ["Segment 0 came back empty."]);
    assert_eq!(
        CleanupDraft {
            segments: vec![CleanupDraftSegment {
                index: 0,
                text: "   \n".to_owned()
            }]
        }
        .problems(&chunk(5)),
        ["Segment 0 came back empty."]
    );
    assert_eq!(problems(0, 4).len(), 0);
    assert_eq!(CleanupDraft::word_count("  a\tb\nc  "), 3);
}

#[test]
fn several_reworded_segments_are_all_reported() {
    let chunk = first_chunk();
    let mut draft = echo(&chunk);
    draft.segments[1].text = "Kurz.".to_owned();
    draft.segments[4].text = String::new();
    let problems = draft.problems(&chunk);
    assert_eq!(problems.len(), 2);
    assert!(problems[0].starts_with("Segment 1 changed from"));
    assert_eq!(problems[1], "Segment 4 came back empty.");
}

#[tokio::test]
async fn a_retry_that_comes_back_right_is_applied_and_not_a_failed_chunk() {
    let server = StubChatServer::start().await.unwrap();
    let chunker = TranscriptChunker::new(150, 220, 3);
    let input = cleanup_input(&standup());
    let chunks = chunker.chunk(&input.segments, input.language.as_ref());
    assert!(chunks.len() >= 3);
    let victim = chunks[1].segments[0].text.clone();
    let victim_for_responder = victim.clone();
    // First answer for the victim chunk drops a segment; the retry is
    // perfect and capitalises every segment so the applied text is
    // recognisable.
    server.respond(Arc::new(move |request| {
        let first_user = request
            .chat
            .as_ref()?
            .messages
            .iter()
            .find(|m| m.role == "user")?
            .content
            .clone();
        let mut segments = parse_segments(&first_user);
        let is_victim = segments
            .first()
            .is_some_and(|(_, text)| *text == victim_for_responder);
        let is_retry = request.purpose.as_deref() == Some("cleanup-retry");
        if is_victim && !is_retry {
            segments.pop();
        }
        let draft = CleanupDraft {
            segments: segments
                .into_iter()
                .map(|(index, text)| CleanupDraftSegment {
                    index,
                    text: capitalised(&text),
                })
                .collect(),
        };
        Some(scripts.json(&draft, Some(default_usage())))
    }));
    let output = cleaner(&server, Some(chunker), |_| {})
        .clean(&input)
        .await
        .unwrap();
    assert_eq!(output.failed_chunks.len(), 0);
    assert!(output.segments.iter().all(|s| starts_uppercase(&s.text)));
    for (out, inp) in output.segments.iter().zip(&input.segments) {
        assert_eq!(out.raw_text, inp.raw_text);
    }
    let requests = server.requests();
    let retries = requests
        .iter()
        .filter(|r| r.purpose.as_deref() == Some("cleanup-retry"))
        .count();
    assert_eq!(retries, 1);
    assert_eq!(requests.len(), chunks.len() + 1);
    assert_eq!(
        output.usage.requests,
        i64::try_from(chunks.len() + 1).unwrap()
    );
    let victim_offset = chunks[0].segments.len();
    assert!(
        output.segments[victim_offset]
            .text
            .starts_with(&capitalised(&victim)[..1])
    );
}

#[tokio::test]
async fn several_failing_chunks_are_listed_in_order_while_the_rest_is_cleaned() {
    let server = StubChatServer::start().await.unwrap();
    let chunker = TranscriptChunker::new(150, 220, 3);
    let input = cleanup_input(&standup());
    let chunks = chunker.chunk(&input.segments, input.language.as_ref());
    assert!(chunks.len() >= 3);
    let first_texts: std::collections::HashSet<String> = [&chunks[0], &chunks[chunks.len() - 1]]
        .iter()
        .map(|c| c.segments[0].text.clone())
        .collect();
    server.respond(Arc::new(move |request| {
        let first_user = request
            .chat
            .as_ref()?
            .messages
            .iter()
            .find(|m| m.role == "user")?
            .content
            .clone();
        let segments = parse_segments(&first_user);
        let sabotage = segments
            .first()
            .is_some_and(|(_, text)| first_texts.contains(text));
        let draft = CleanupDraft {
            segments: segments
                .into_iter()
                .map(|(index, text)| CleanupDraftSegment {
                    index,
                    // Halves every segment: a word-ratio failure both times.
                    text: if sabotage {
                        words(1)
                    } else {
                        capitalised(&text)
                    },
                })
                .collect(),
        };
        Some(scripts.json(&draft, Some(default_usage())))
    }));
    let output = cleaner(&server, Some(chunker), |_| {})
        .clean(&input)
        .await
        .unwrap();
    assert_eq!(output.failed_chunks, [0, chunks.len() - 1]);
    assert_eq!(output.segments.len(), input.segments.len());
    let mut offset = 0;
    for chunk in &chunks {
        let texts: Vec<&str> = output.segments[offset..offset + chunk.segments.len()]
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        if output.failed_chunks.contains(&chunk.index) {
            let raw: Vec<&str> = chunk.segments.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(texts, raw, "chunk {} kept raw", chunk.index);
        } else {
            assert!(
                texts.iter().all(|t| starts_uppercase(t)),
                "chunk {} cleaned",
                chunk.index
            );
        }
        offset += chunk.segments.len();
    }
    assert_eq!(server.request_count(), chunks.len() + 2);
    let retry = server
        .requests()
        .into_iter()
        .find(|r| r.purpose.as_deref() == Some("cleanup-retry"))
        .unwrap()
        .chat
        .unwrap();
    assert!(
        retry
            .messages
            .last()
            .unwrap()
            .content
            .contains("keep the wording")
    );
}

#[tokio::test]
async fn a_refusal_falls_back_to_raw_after_one_retry() {
    let server = StubChatServer::start().await.unwrap();
    server.enqueue([
        scripts.refusal("I will not edit this."),
        scripts.refusal("Still no."),
    ]);
    let standup = standup();
    let output = cleaner(&server, None, |_| {})
        .clean(&cleanup_input(&standup))
        .await
        .unwrap();
    assert_eq!(output.failed_chunks, [0]);
    assert_eq!(output.segments, standup.segments);
    assert_eq!(server.request_count(), 2);
    let retry = server.requests().pop().unwrap().chat.unwrap();
    assert_eq!(
        retry.messages.len(),
        4,
        "the retry carries the rejected answer and the reason"
    );
    assert_eq!(retry.messages[2].role, "assistant");
    assert!(
        retry.messages[3]
            .content
            .contains("the model refused: I will not edit this.")
    );
    assert_eq!(output.usage.requests, 2, "a refusal still cost a request");
}
