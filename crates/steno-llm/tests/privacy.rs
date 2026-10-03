//! Invariant 3 of the plan on the wire and on disk: each claim of the crate
//! doc's privacy paragraph, asserted for every client it applies to:
//! - a completion body is a JSON document of prompt text, with no file
//!   path, audio byte, speaker or meeting id, or segment `raw_text`;
//! - secrets travel only in headers and in the token refresh: the API key
//!   read from the `SecretStore` only in `Authorization`, the Codex access
//!   token and account id only in their two headers, the refresh token only
//!   in the refresh's body;
//! - the tokens read from `auth.json` are written back to it after a
//!   refresh with mode 0600 on Unix, leaving no temporary file;
//! - no secret, however often or wherever in a body a server echoes it,
//!   reaches an error, a `Debug` form or an observer event, since a body is
//!   redacted whole before it is cut;
//! - a key shorter than eight bytes is a placeholder and is left as it is.
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
async fn the_endpoint_client_and_both_passes_send_text_only_and_the_key_only_in_authorization() {
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
async fn the_codex_client_sends_text_only_and_the_access_token_and_account_id_only_in_their_headers()
 {
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
/// file it writes back is 0600 on Unix and nothing else is left in the home.
#[tokio::test]
async fn the_refresh_token_goes_only_in_the_refresh_body_and_auth_json_is_written_back_0600() {
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

/// Fails when `text` holds eight bytes in a row of any secret, in any case:
/// a cut taken before the redaction leaves a prefix of the secret behind,
/// a lowercased field a case-folded copy.
fn assert_no_secret(text: &str, secrets: &[String], what: &str) {
    let folded = text.to_lowercase();
    for secret in secrets {
        let secret = secret.to_lowercase();
        for piece in secret.as_bytes().windows(secret.len().min(8)) {
            let piece = std::str::from_utf8(piece).expect("ASCII secrets");
            assert!(
                !folded.contains(piece),
                "{what} carries part of a secret ({piece}): {text}"
            );
        }
    }
}

fn assert_error_redacted(error: &(impl std::fmt::Display + std::fmt::Debug), secrets: &[String]) {
    assert_no_secret(&error.to_string(), secrets, "Display");
    assert_no_secret(&format!("{error:?}"), secrets, "Debug");
}

/// The Codex secrets of `AuthFile::default()` with `access` in it.
fn codex_secrets(access: &str) -> Vec<String> {
    vec![
        access.to_owned(),
        "rt_original".to_owned(),
        "acct_stored".to_owned(),
    ]
}

/// Each secret twice in the error message, once as an echoed header and
/// once in a nested field.
fn echoing_envelope(secrets: &[String]) -> serde_json::Value {
    let all = secrets.join(" and ");
    serde_json::json!({
        "error": {
            "message": format!("rejected {all}; seen again: {all}"),
            "type": "invalid_request_error",
        },
        "echo": {"headers": {"authorization": format!("Bearer {}", secrets[0])}},
        "nested": {"deep": [secrets]},
    })
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
        StubResponse::json(&echoing_envelope(&secrets), 400)
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
    let secrets = codex_secrets(&access);
    let all = secrets.join(" and ");
    harness.backend.enqueue([
        StubResponse::json(&echoing_envelope(&secrets), 400),
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
    let secrets = codex_secrets(&access);
    let all = secrets.join(" and ");
    let nested = StubResponse::json(&echoing_envelope(&secrets), 400);
    let flat = StubResponse::json(
        &serde_json::json!({
            "error": "acct_stored",
            "error_code": "rt_original",
            "error_description": format!("{all}; again {all}"),
            "echo": {"authorization": format!("Bearer {access}")},
        }),
        400,
    );
    // A code is lowercased for the decisions; a token in it is redacted
    // before that, or a case-folded copy would survive.
    let cased = StubResponse::json(&serde_json::json!({"error": {"code": access}}), 400);
    let plain = StubResponse::new(503, echoing_plain_body(&secrets[1..], &access, 300));
    for reply in [nested, flat, cased, plain] {
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

// Every cut a client takes: 4 096 characters of a body that is not the
// answer, 500 of an error body that is not an envelope, 300 of a refusal
// from the token endpoint. Each body is redacted whole before it is cut.

const BODY_CUT: usize = 4_096;
const ERROR_CUT: usize = 500;
const REFRESH_CUT: usize = 300;

/// A project key (`sk-proj-` and 160 more characters), long enough that a
/// body of its copies redacts to fewer than 500 characters before byte
/// 4 096.
fn long_api_key() -> String {
    format!("sk-proj-{}", "A1b2C3d4".repeat(20))
}

/// `filler` up to the cut, then `secret` with all but its last byte before
/// the cut, then more text. `in_bytes` places the cut at a byte offset
/// (the body cut of the old code), else at a character offset; a
/// multi-byte `filler` ends right where the secret starts.
fn straddling(filler: char, secret: &str, cut: usize, in_bytes: bool) -> Vec<u8> {
    let before = cut - (secret.len() - 1);
    let mut body = if in_bytes {
        let width = filler.len_utf8();
        "x".repeat(before % width) + &filler.to_string().repeat(before / width)
    } else {
        filler.to_string().repeat(before)
    };
    body.push_str(secret);
    body.push_str(" trailing");
    body.into_bytes()
}

/// Copies of `secret`, one of them across byte 4 096 and five more after
/// it, so the body runs past every cut.
fn many_copies(secret: &str) -> Vec<u8> {
    let start = BODY_CUT - (secret.len() - 1);
    let mut body = String::new();
    while body.len() + secret.len() < start {
        body.push_str(secret);
        body.push(' ');
    }
    body.push_str(&"x".repeat(start - body.len()));
    for _ in 0..6 {
        body.push_str(secret);
        body.push(' ');
    }
    body.into_bytes()
}

/// Each body twice to an endpoint client allowed two attempts, `status` 200
/// for a body that is not a completion and 500 for one that is not an
/// error envelope: the first error lands in a `Retrying` event, the second
/// is returned.
async fn assert_endpoint_client_redacts(key: &str, bodies: &[(u16, Vec<u8>)]) {
    let secrets = vec![key.to_owned()];
    let harness = ClientHarness::build(RetryPolicy::with_max_attempts(2), Some(key), |_| {}).await;
    let driver = harness.drive_retries();
    for (status, body) in bodies {
        let reply = StubResponse::new(*status, body.clone());
        harness.server.enqueue([reply.clone(), reply]);
        let error = harness
            .client
            .complete_llm(&text_request())
            .await
            .unwrap_err();
        assert_error_redacted(&error, &secrets);
        assert!(error.to_string().contains("[redacted]"), "{error}");
    }
    driver.abort();
    let events = harness.events();
    assert_eq!(
        events.iter().filter(|e| is_retrying(e)).count(),
        bodies.len()
    );
    for event in events {
        assert_no_secret(&format!("{event:?}"), &secrets, "an event");
    }
}

/// As [`assert_endpoint_client_redacts`] for the Codex client, and each body
/// once more as the answer to the model list.
async fn assert_codex_client_redacts(bodies: &[(u16, Vec<u8>)]) {
    let harness = CodexHarness::with_retry(RetryPolicy::with_max_attempts(2)).await;
    let secrets = codex_secrets(&CodexHome::access_token(3_600, "plus"));
    let driver = harness.drive_retries();
    for (status, body) in bodies {
        let reply = StubResponse::new(*status, body.clone());
        harness
            .backend
            .enqueue([reply.clone(), reply.clone(), reply]);
        let error = harness
            .client
            .complete_llm(&text_request())
            .await
            .unwrap_err();
        assert_error_redacted(&error, &secrets);
        assert!(error.to_string().contains("[redacted]"), "{error}");
        let error = harness.client.list_models().await.unwrap_err();
        assert_error_redacted(&error, &secrets);
        assert!(error.to_string().contains("[redacted]"), "{error}");
    }
    driver.abort();
    let events = harness.events();
    assert_eq!(
        events.iter().filter(|e| is_retrying(e)).count(),
        bodies.len()
    );
    for event in events {
        assert_no_secret(&format!("{event:?}"), &secrets, "an event");
    }
}

/// Each body as the token endpoint's 503 to a refresh of a stale sign-in.
async fn assert_token_refresh_redacts(bodies: impl Fn(&[String]) -> Vec<Vec<u8>>) {
    let access = CodexHome::access_token(10, "plus");
    let secrets = codex_secrets(&access);
    for body in bodies(&secrets) {
        let home = CodexHome::new().await;
        home.write(AuthFile::default().access(&access));
        home.server.enqueue([StubResponse::new(503, body)]);
        let error = home.store().current().await.unwrap_err();
        assert_error_redacted(&error, &secrets);
        let detail = error.detail().unwrap();
        assert_no_secret(detail, &secrets, "the detail");
        assert!(detail.contains("[redacted]"), "{detail}");
    }
}

#[tokio::test]
async fn no_secret_straddling_the_4096_byte_cut_reaches_an_error_or_an_event() {
    let key = long_api_key();
    assert_endpoint_client_redacts(API_KEY, &[(200, straddling('x', API_KEY, BODY_CUT, true))])
        .await;
    assert_endpoint_client_redacts(&key, &[(200, straddling('x', &key, BODY_CUT, true))]).await;
    let secrets = codex_secrets(&CodexHome::access_token(3_600, "plus"));
    let bodies: Vec<(u16, Vec<u8>)> = secrets
        .iter()
        .map(|secret| (200, straddling('x', secret, BODY_CUT, true)))
        .collect();
    assert_codex_client_redacts(&bodies).await;
    assert_token_refresh_redacts(|secrets| {
        secrets
            .iter()
            .map(|s| straddling('x', s, REFRESH_CUT, false))
            .collect()
    })
    .await;
}

#[tokio::test]
async fn no_secret_echoed_past_every_cut_reaches_an_error_or_an_event() {
    for key in [API_KEY.to_owned(), long_api_key()] {
        assert_endpoint_client_redacts(&key, &[(200, many_copies(&key)), (500, many_copies(&key))])
            .await;
    }
    let secrets = codex_secrets(&CodexHome::access_token(3_600, "plus"));
    let bodies: Vec<(u16, Vec<u8>)> = secrets
        .iter()
        .flat_map(|secret| [(200, many_copies(secret)), (500, many_copies(secret))])
        .collect();
    assert_codex_client_redacts(&bodies).await;
    assert_token_refresh_redacts(|secrets| secrets.iter().map(|s| many_copies(s)).collect()).await;
}

#[tokio::test]
async fn no_secret_after_a_multi_byte_character_at_a_cut_reaches_an_error_or_an_event() {
    let key = long_api_key();
    for key in [API_KEY, key.as_str()] {
        assert_endpoint_client_redacts(
            key,
            &[
                (200, straddling('€', key, BODY_CUT, true)),
                (500, straddling('€', key, ERROR_CUT, false)),
            ],
        )
        .await;
    }
    let secrets = codex_secrets(&CodexHome::access_token(3_600, "plus"));
    let bodies: Vec<(u16, Vec<u8>)> = secrets
        .iter()
        .flat_map(|secret| {
            [
                (200, straddling('€', secret, BODY_CUT, true)),
                (500, straddling('€', secret, ERROR_CUT, false)),
            ]
        })
        .collect();
    assert_codex_client_redacts(&bodies).await;
    assert_token_refresh_redacts(|secrets| {
        secrets
            .iter()
            .map(|s| straddling('€', s, REFRESH_CUT, false))
            .collect()
    })
    .await;
}

/// A key shorter than eight bytes is a placeholder for a local server, not
/// a credential, and is left as it is: redacting it would garble every
/// message. Eight bytes and more is a secret.
#[tokio::test]
async fn a_key_shorter_than_eight_bytes_is_a_placeholder_and_left_as_it_is() {
    let message = serde_json::json!({"error": {"message": "context exceeded max tokens"}});
    for (key, expected) in [
        ("x", "HTTP 400: context exceeded max tokens"),
        ("ollama", "HTTP 400: context exceeded max tokens"),
        ("exceeded", "HTTP 400: context [redacted] max tokens"),
    ] {
        let harness = ClientHarness::build(RetryPolicy::NONE, Some(key), |_| {}).await;
        harness.server.enqueue([StubResponse::json(&message, 400)]);
        let error = harness
            .client
            .complete_llm(&text_request())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), expected, "{key}");
    }
}
