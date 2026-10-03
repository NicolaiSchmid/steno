//! Invariant 3 of the plan as seen on the wire, one test per claim of the
//! privacy paragraph in the crate doc:
//! - a completion body is a JSON document of prompt text, with no file
//!   path, audio byte, speaker or meeting id, or segment `raw_text`;
//! - secrets travel only in headers and in the token refresh: the API key
//!   from the `SecretStore` in `Authorization`, the Codex access token and
//!   account id in their two headers, the refresh token only in the
//!   refresh's body;
//! - the refreshed `auth.json` is written back with mode 0600 (unix) and
//!   leaves no temporary file;
//! - no secret, however often a server echoes it, reaches an error, a
//!   `Debug` form or an observer event.
//!
//! Swift: no single suite; Rust-only.

mod common;

use std::sync::Arc;

use chrono::Utc;
use common::*;
use steno_core::testing::InMemorySecretStore;
use steno_core::{MeetingSummarizer, SecretKey, SummaryTemplate, TranscriptCleaner};
use steno_llm::inputs::{cleanup_input, summary_input};
use steno_llm::testing::{RecordedRequest, StubChatServer, StubResponse, scripts};
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

/// The names of the headers whose value holds `secret`, sorted.
fn headers_carrying(request: &RecordedRequest, secret: &str) -> Vec<String> {
    let mut names: Vec<String> = request
        .headers
        .iter()
        .filter(|(_, value)| value.contains(secret))
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    names
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
    let secrets = InMemorySecretStore::with([(SecretKey::llm_api_key(), API_KEY.to_owned())]);
    let client = Arc::new(
        OpenAiCompatibleClient::from_secret_store(endpoint.clone(), &secrets)
            .await
            .unwrap()
            .with_retry(RetryPolicy::NONE),
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
    // The key from the secret store goes in the Authorization header and in
    // no other header or body; the summary reads the cleaned text, never
    // rawText.
    for request in &requests {
        assert_eq!(
            headers_carrying(request, API_KEY),
            ["authorization"],
            "{}",
            request.path
        );
        assert_eq!(
            request.authorization(),
            Some(format!("Bearer {API_KEY}").as_str())
        );
    }
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
        assert!(!request.body_text().contains("rt_original"));
        assert_eq!(headers_carrying(request, &access), ["authorization"]);
        assert_eq!(
            headers_carrying(request, "acct_stored"),
            ["chatgpt-account-id"]
        );
        assert_eq!(
            headers_carrying(request, "rt_original"),
            Vec::<String>::new()
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

/// The refresh token goes to the token endpoint in the refresh's body and
/// nowhere else; the refresh carries no access token or account id; the
/// file it writes back is 0600 and nothing else is left in the home.
#[tokio::test]
async fn the_refresh_token_goes_only_to_the_token_endpoint_and_the_file_stays_0600() {
    let harness = CodexHarness::build(RetryPolicy::NONE, "gpt-stub", |_| {}).await;
    let stale = CodexHome::access_token(10, "plus");
    harness.home.write(AuthFile::default().access(&stale));
    let fresh = CodexHome::access_token(3_600, "plus");
    harness
        .home
        .server
        .enqueue([scripts.token_refresh(&fresh, Some("rt_rotated"), None)]);
    harness.backend.enqueue([scripts.stream("ok")]);
    harness.client.complete_llm(&text_request()).await.unwrap();

    let [refresh] = harness.home.server.requests().try_into().unwrap();
    let body: serde_json::Value = serde_json::from_slice(&refresh.body).unwrap();
    assert_eq!(
        body,
        serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": "app_test",
            "refresh_token": "rt_original",
        })
    );
    assert_eq!(refresh.authorization(), None);
    for secret in [stale.as_str(), "acct_stored", "rt_original"] {
        assert_eq!(headers_carrying(&refresh, secret), Vec::<String>::new());
    }
    for request in harness.backend.requests() {
        for refresh_token in ["rt_original", "rt_rotated"] {
            assert!(!request.body_text().contains(refresh_token));
            assert_eq!(
                headers_carrying(&request, refresh_token),
                Vec::<String>::new()
            );
        }
        assert_eq!(headers_carrying(&request, &fresh), ["authorization"]);
    }

    let entries: Vec<String> = std::fs::read_dir(harness.home.directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, ["auth.json"], "no temporary file is left");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(harness.home.file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

// No secret reaches an error, a `Debug` form or an observer event, however
// often and wherever a server echoes it.

/// Fails when `text` holds any secret, or the first half of a long one (a
/// cut that kept part of it).
fn assert_no_secret(text: &str, secrets: &[String], what: &str) {
    for secret in secrets {
        assert!(
            !text.contains(secret.as_str()),
            "{what} carries a secret: {text}"
        );
        if secret.len() >= 16 {
            let half = &secret[..secret.len() / 2];
            assert!(
                !text.contains(half),
                "{what} carries part of a secret: {text}"
            );
        }
    }
}

fn assert_error_redacted(error: &(impl std::fmt::Display + std::fmt::Debug), secrets: &[String]) {
    assert_no_secret(&error.to_string(), secrets, "Display");
    assert_no_secret(&format!("{error:?}"), secrets, "Debug");
}

/// Each secret twice in the error message, once as an echoed header and
/// once in a nested field.
fn echoing_envelope(secrets: &[String]) -> String {
    let all = secrets.join(" and ");
    serde_json::json!({
        "error": {
            "message": format!("rejected {all}; seen again: {all}"),
            "type": "invalid_request_error",
        },
        "echo": {"headers": {"authorization": format!("Bearer {}", secrets[0])}},
        "nested": {"deep": [secrets]},
    })
    .to_string()
}

/// A body that is not an error envelope, so the client falls back to its
/// first `limit` characters: `echoed` as a header echo and in a nested
/// field, then `cut` placed so it straddles the limit.
fn echoing_plain_body(echoed: &[String], cut: &str, limit: usize) -> Vec<u8> {
    let mut body = format!(
        "Authorization: Bearer {}\n{}\n",
        echoed[0],
        serde_json::json!({"nested": {"deep": echoed}})
    );
    let start = limit - cut.len() / 2;
    assert!(body.len() < start, "the echo fits before the cut");
    body.push_str(&"x".repeat(start - body.len()));
    body.push_str(cut);
    body.push_str(" trailing");
    body.into_bytes()
}

#[tokio::test]
async fn the_endpoint_client_redacts_every_copy_of_the_api_key() {
    let secrets = vec![API_KEY.to_owned()];
    let harness = ClientHarness::with_retry(RetryPolicy::NONE).await;
    let twice = format!("{API_KEY} and {API_KEY}");
    harness.server.enqueue([
        StubResponse::new(400, echoing_envelope(&secrets).into_bytes())
            .with_header("Content-Type", "application/json")
            .with_header("X-Echo", &format!("Bearer {API_KEY}")),
        StubResponse::new(400, echoing_plain_body(&secrets, API_KEY, 500)),
        scripts.refusal(&format!("I saw {twice}")),
        StubResponse::new(200, format!("not a completion: {twice}").into_bytes()),
    ]);
    for _ in 0..4 {
        let error = harness
            .client
            .complete_llm(&text_request())
            .await
            .unwrap_err();
        assert_error_redacted(&error, &secrets);
        assert!(error.to_string().contains("[redacted]"), "{error}");
    }
    for event in harness.events() {
        assert_no_secret(&format!("{event:?}"), &secrets, "an event");
    }
    assert_no_secret(&format!("{:?}", harness.client), &secrets, "the client");
}

#[tokio::test]
async fn the_codex_client_redacts_every_copy_of_the_tokens_and_the_account_id() {
    let harness = CodexHarness::build(RetryPolicy::NONE, "gpt-stub", |_| {}).await;
    let access = CodexHome::access_token(3_600, "plus");
    let secrets = vec![
        access.clone(),
        "rt_original".to_owned(),
        "acct_stored".to_owned(),
    ];
    let all = secrets.join(" and ");
    harness.backend.enqueue([
        StubResponse::new(400, echoing_envelope(&secrets).into_bytes())
            .with_header("Content-Type", "application/json"),
        StubResponse::new(400, echoing_plain_body(&secrets[1..], &access, 500)),
        scripts.responses_error_event(&format!("{all}; again {all}"), "server_error"),
        scripts.responses_refusal(&format!("I saw {all}; again {all}")),
        StubResponse::new(200, format!("<html>{all}; again {all}</html>").into_bytes()),
    ]);
    for _ in 0..5 {
        let error = harness
            .client
            .complete_llm(&text_request())
            .await
            .unwrap_err();
        assert_error_redacted(&error, &secrets);
        assert!(error.to_string().contains("[redacted]"), "{error}");
    }
    for event in harness.events() {
        assert_no_secret(&format!("{event:?}"), &secrets, "an event");
    }
    let credentials = harness.home.store().stored().unwrap();
    assert_no_secret(&format!("{credentials:?}"), &secrets, "the credentials");
}

#[tokio::test]
async fn the_token_refresh_redacts_every_copy_of_the_tokens_and_the_account_id() {
    let access = CodexHome::access_token(10, "plus");
    let secrets = vec![
        access.clone(),
        "rt_original".to_owned(),
        "acct_stored".to_owned(),
    ];
    let all = secrets.join(" and ");
    let nested = StubResponse::new(400, echoing_envelope(&secrets).into_bytes())
        .with_header("Content-Type", "application/json");
    let flat = StubResponse::new(
        400,
        serde_json::json!({
            "error": "acct_stored",
            "error_code": "rt_original",
            "error_description": format!("{all}; again {all}"),
            "echo": {"authorization": format!("Bearer {access}")},
        })
        .to_string()
        .into_bytes(),
    )
    .with_header("Content-Type", "application/json");
    let plain = StubResponse::new(503, echoing_plain_body(&secrets[1..], &access, 300));
    for reply in [nested, flat, plain] {
        let home = CodexHome::new().await;
        home.write(AuthFile::default().access(&access));
        home.server.enqueue([reply]);
        let error = home.store().current().await.unwrap_err();
        assert_error_redacted(&error, &secrets);
        let detail = error.detail().unwrap();
        assert_no_secret(detail, &secrets, "the detail");
        assert!(detail.contains("[redacted]"), "{detail}");
    }
}
