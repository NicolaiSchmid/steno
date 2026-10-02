//! Invariant 3 of the plan as seen on the wire: both clients and both
//! passes send text and nothing else. Every request body the stub servers
//! record is a JSON document of prompt text; no file path, no audio byte,
//! no speaker or meeting id, no raw transcript, no API key and no refresh
//! token ever appears in it.

mod common;

use std::sync::Arc;

use chrono::Utc;
use common::*;
use steno_core::{MeetingSummarizer, SummaryTemplate, TranscriptCleaner};
use steno_llm::inputs::{cleanup_input, summary_input};
use steno_llm::testing::{RecordedRequest, StubChatServer, scripts};
use steno_llm::{
    LlmEndpoint, LlmMeetingSummarizer, LlmTranscriptCleaner, OpenAiCompatibleClient, RetryPolicy,
    TranscriptChunker,
};

/// What must never leave the device, as it would appear in a body.
fn forbidden(export: &steno_core::MeetingExport) -> Vec<String> {
    let mut forbidden = vec![
        "/Users/".to_owned(),
        "file://".to_owned(),
        ".wav".to_owned(),
        ".m4a".to_owned(),
        ".caf".to_owned(),
        "RAW-".to_owned(),
        API_KEY.to_owned(),
        "rt_original".to_owned(),
        steno_core::json::uuid_string(export.meeting.id),
    ];
    forbidden.extend(
        export
            .speakers
            .iter()
            .map(|s| steno_core::json::uuid_string(s.id)),
    );
    forbidden.extend(
        export
            .segments
            .iter()
            .take(5)
            .map(|s| steno_core::json::uuid_string(s.id)),
    );
    forbidden.extend(
        export
            .persons
            .iter()
            .map(|p| steno_core::json::uuid_string(p.id)),
    );
    forbidden
}

fn assert_text_only(requests: &[RecordedRequest], export: &steno_core::MeetingExport) {
    assert_ne!(requests.len(), 0);
    let forbidden = forbidden(export);
    for request in requests {
        let body = request.body_text();
        let json: serde_json::Value =
            serde_json::from_str(&body).expect("every body is a JSON document");
        assert!(json.is_object(), "{}", request.path);
        for needle in &forbidden {
            assert!(!body.contains(needle), "{} carries {needle}", request.path);
        }
        // Every string in the body is valid text; a body is never bytes.
        assert!(
            body.chars()
                .all(|c| !c.is_control() || c == '\n' || c == '\t' || c == '\r'),
            "{} carries control bytes",
            request.path
        );
        // The clients send prompt text, not the audio asset or paths.
        assert!(!body.contains("\"audio\""));
        assert!(!body.contains("sampleClipURL"));
        assert!(!body.contains("fileURL"));
    }
}

/// An export whose raw text, audio asset and clip URLs would stand out in a
/// body: the pass reads the cleaned text and never the asset.
fn sensitive_export() -> steno_core::MeetingExport {
    let mut export = standup();
    for (index, segment) in export.segments.iter_mut().enumerate() {
        segment.raw_text = format!("RAW-{index}");
    }
    for speaker in &mut export.speakers {
        speaker.sample_clip_url = Some("file:///Users/nicolai/Library/clip.wav".to_owned());
    }
    export.audio = Some(steno_core::AudioAsset {
        id: sample_uuid(900),
        meeting_id: export.meeting.id,
        url: "file:///Users/nicolai/Library/Application Support/Steno/Audio/master.caf".to_owned(),
        format: steno_core::AudioFormat::Caf48kFloat32,
        lanes: vec![steno_core::AudioLane::Mixed],
        sidecars_16k: std::iter::once((
            steno_core::AudioLane::Mixed,
            "file:///Users/nicolai/Library/mixed-16k.wav".to_owned(),
        ))
        .collect(),
        mixdown_url: Some("file:///Users/nicolai/Library/mix.m4a".to_owned()),
        retention: steno_core::AudioRetention::KeepForever,
        expires_at: None,
    });
    export
}

#[tokio::test]
async fn the_endpoint_client_and_both_passes_send_text_only() {
    let export = sensitive_export();
    let server = StubChatServer::start().await.unwrap();
    server.respond(Arc::new(move |request| {
        Some(match request.purpose.as_deref() {
            Some("cleanup" | "cleanup-retry") => scripts
                .cleanup_echo(steno_llm::testing::default_usage(), |_, t| {
                    Some(t.to_owned())
                })(request)
            .unwrap(),
            _ => scripts.text(&canned("summary-default-standup")),
        })
    }));
    let endpoint = LlmEndpoint::new(server.base_url().clone(), "stub-model");
    let client = Arc::new(
        OpenAiCompatibleClient::new(endpoint.clone(), Some(API_KEY)).with_retry(RetryPolicy::NONE),
    );
    let cleaner = LlmTranscriptCleaner::new(client.clone(), endpoint.clone())
        .with_chunker(TranscriptChunker::new(150, 220, 3));
    let output = cleaner.clean(&cleanup_input(&export)).await.unwrap();
    assert_eq!(output.failed_chunks.len(), 0);
    let summarizer = LlmMeetingSummarizer::new(client, endpoint, Utc);
    summarizer
        .summarize(&summary_input(
            &export,
            SummaryTemplate::bundled_with_id("default"),
        ))
        .await
        .unwrap();

    let requests = server.requests();
    assert!(requests.len() >= 3);
    assert_text_only(&requests, &export);
    // The Authorization header carries the key, the body never does; the
    // summary reads the cleaned text, never rawText.
    assert!(
        requests
            .iter()
            .all(|r| r.authorization() == Some(&format!("Bearer {API_KEY}")))
    );
    let summary = requests
        .iter()
        .find(|r| r.purpose.as_deref() == Some("summary"))
        .unwrap();
    assert!(
        summary
            .body_text()
            .contains("Speaker 1: okay lass uns anfangen.")
    );
}

#[tokio::test]
async fn the_codex_client_sends_text_only_and_keeps_the_tokens_in_headers() {
    let export = sensitive_export();
    let harness = CodexHarness::new().await;
    harness.backend.respond(Arc::new(move |request| {
        Some(match request.purpose.as_deref() {
            Some("cleanup" | "cleanup-retry") => {
                // The Responses body carries the user turn under `input`.
                let user = request
                    .responses
                    .as_ref()?
                    .input
                    .iter()
                    .rev()
                    .find(|item| item.role == "user")?
                    .content[0]
                    .text
                    .clone();
                let segments = steno_llm::testing::parse_segments(&user);
                let draft = steno_llm::cleanup::CleanupDraft {
                    segments: segments
                        .into_iter()
                        .map(|(index, text)| steno_llm::cleanup::CleanupDraftSegment {
                            index,
                            text,
                        })
                        .collect(),
                };
                scripts.stream(&serde_json::to_string(&draft).unwrap())
            }
            _ => scripts.stream(&canned("summary-default-standup")),
        })
    }));
    let endpoint = harness.endpoint.clone();
    let client: Arc<dyn steno_core::LanguageModel> = Arc::new(
        steno_llm::CodexResponsesClient::new(endpoint.clone(), Arc::new(harness.home.store()))
            .with_retry(RetryPolicy::NONE),
    );
    let cleaner = LlmTranscriptCleaner::new(client.clone(), endpoint.clone())
        .with_chunker(TranscriptChunker::new(150, 220, 3));
    let output = cleaner.clean(&cleanup_input(&export)).await.unwrap();
    assert_eq!(output.failed_chunks.len(), 0);
    LlmMeetingSummarizer::new(client, endpoint, Utc)
        .summarize(&summary_input(
            &export,
            SummaryTemplate::bundled_with_id("default"),
        ))
        .await
        .unwrap();

    let requests = harness.backend.requests();
    assert_text_only(&requests, &export);
    let access = CodexHome::access_token(3_600, "plus");
    for request in &requests {
        assert!(
            !request.body_text().contains(&access),
            "the access token stays in the header"
        );
        assert!(
            !request.body_text().contains("acct_stored"),
            "the account id stays in the header"
        );
        assert_eq!(
            request.authorization(),
            Some(format!("Bearer {access}").as_str())
        );
    }
    // The token endpoint was never needed: nothing but the backend saw a
    // request, and the refresh token never went anywhere.
    assert_eq!(harness.home.server.requests().len(), 0);
}
