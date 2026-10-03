//! The property behind the redaction claim of the crate doc's privacy
//! paragraph: no secret, however often or wherever in a body a server
//! echoes it, reaches an error, a `Debug` form or an observer event.
//! Random secrets of the four kinds (API key, access token, refresh token,
//! account id), 8 to 200 characters, some with multi-byte characters, go
//! into random bodies that echo them one to four times, some copies across
//! the 300- and 500-character cuts and byte 4 096, and every reply shape
//! that can carry text into an error: plain and enveloped error bodies,
//! undecodable 2xx bodies, refusals, stream errors, the model list, a 401
//! whose refresh fails, and the token endpoint's refusals with the secret
//! in the code. The seed is fixed, so a failure always reproduces.
//!
//! Swift: no single suite; Rust-only.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use steno_llm::testing::{ManualClock, StubChatServer, StubResponse, scripts};
use steno_llm::{
    CodexCredentialError, CodexError, CodexResponsesClient, LlmEndpoint, OpenAiCompatibleClient,
    RetryPolicy,
};

/// Cases per client and for the token refresh.
const CASES: u64 = 200;

/// Endpoint clients, each with its own random key, the cases take turns
/// on: building a client loads the platform's root certificates, too slow
/// to do two hundred times. The Codex client and the store read the
/// secrets from the file on every call, so one of each serves every case.
const ENDPOINT_CLIENTS: u64 = 8;

/// xorshift64: deterministic and dependency-free.
struct Rng(u64);

impl Rng {
    fn seeded(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            usize::try_from(self.next() % n as u64).unwrap()
        }
    }

    fn range(&mut self, low: usize, high: usize) -> usize {
        low + self.below(high - low + 1)
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }
}

const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const MULTI_BYTE: &[char] = &['ü', '日', '🎉', '€'];
const FILLER: &[char] = &[
    'a', 'b', 'q', ' ', ' ', '.', ',', ':', '{', '}', '"', '\\', '\n', 'x', 'ü', '日', '🎉', 'é',
    '0', '9',
];

/// A secret of 8 to 200 characters: base64url, JWT-like with dots, or with
/// a few multi-byte characters.
fn secret(rng: &mut Rng) -> String {
    let length = match rng.below(4) {
        0 => rng.range(8, 12),
        1 => rng.range(8, 40),
        _ => rng.range(8, 200),
    };
    let style = rng.below(3);
    (0..length)
        .map(|index| match style {
            1 if index > 0 && rng.below(20) == 0 => '.',
            2 if rng.below(10) == 0 => rng.pick(MULTI_BYTE),
            _ => char::from(rng.pick(ALPHABET)),
        })
        .collect()
}

/// Up to `max` characters of filler holding one to four copies of the
/// secrets, some placed across the 300- and 500-character cuts, and now
/// and then padded so a copy straddles byte 4 096.
fn body_with(rng: &mut Rng, secrets: &[String], max: usize) -> String {
    let length = rng.below(max + 1);
    let mut chars: Vec<char> = (0..length).map(|_| rng.pick(FILLER)).collect();
    let mut copies: Vec<(usize, &String)> = (0..rng.range(1, 4))
        .map(|_| {
            let secret = &secrets[rng.below(secrets.len())];
            let inside = rng.range(1, secret.chars().count());
            let offset = match rng.below(5) {
                0 => 300usize.saturating_sub(inside),
                1 => 500usize.saturating_sub(inside),
                // After "HTTP 4xx: " in the refresh's detail.
                2 => 290usize.saturating_sub(inside),
                _ => rng.below(chars.len() + 1),
            };
            (offset, secret)
        })
        .collect();
    copies.sort_by_key(|(offset, _)| *offset);
    let mut floor = 0;
    for (offset, secret) in copies {
        let at = offset.max(floor).min(chars.len());
        chars.splice(at..at, secret.chars());
        floor = at + secret.chars().count();
    }
    let mut text: String = chars.into_iter().collect();
    if rng.below(6) == 0 {
        let secret = &secrets[rng.below(secrets.len())];
        let target = 4_096usize.saturating_sub(rng.range(1, secret.len()));
        while text.len() < target {
            text.push('z');
        }
        text.push_str(secret);
        text.push_str(" tail");
    }
    text
}

/// What `text` keeps of any secret: its first eight characters or more, in
/// any case.
fn leaks(text: &str, secrets: &[String], what: &str) -> Vec<String> {
    let folded = text.to_lowercase();
    secrets
        .iter()
        .filter(|secret| {
            let prefix: String = secret.chars().take(8).collect();
            folded.contains(&prefix.to_lowercase())
        })
        .map(|secret| {
            let shown: String = text.chars().take(200).collect();
            format!("{what} keeps a prefix of {secret}: {shown}")
        })
        .collect()
}

fn error_leaks(
    error: &(impl std::fmt::Display + std::fmt::Debug),
    secrets: &[String],
) -> Vec<String> {
    let mut found = leaks(&error.to_string(), secrets, "Display");
    found.extend(leaks(&format!("{error:?}"), secrets, "Debug"));
    found
}

fn credential_leaks(error: &CodexCredentialError, secrets: &[String]) -> Vec<String> {
    let mut found = error_leaks(error, secrets);
    if let Some(detail) = error.detail() {
        found.extend(leaks(detail, secrets, "detail"));
    }
    found
}

/// Two attempts without a wait, so the first error lands in a `Retrying`
/// event.
fn two_attempts() -> RetryPolicy {
    RetryPolicy::new(2, Duration::ZERO, Duration::ZERO)
}

fn always(response: StubResponse) -> steno_llm::testing::Responder {
    Arc::new(move |_| Some(response.clone()))
}

fn error_status(rng: &mut Rng) -> u16 {
    rng.pick(&[400, 403, 404, 409, 422, 500, 502, 503])
}

fn codex_secrets(rng: &mut Rng) -> Vec<String> {
    vec![secret(rng), secret(rng), secret(rng)]
}

fn signed_in(secrets: &[String], stale: bool) -> AuthFile {
    AuthFile {
        access: secrets[0].clone(),
        refresh: secrets[1].clone(),
        id: None,
        account_id: Some(secrets[2].clone()),
        last_refresh: Some(codex_now() - chrono::TimeDelta::days(if stale { 9 } else { 0 })),
        ..AuthFile::default()
    }
}

/// A refusal from the token endpoint, the secrets in its body or code.
fn refresh_refusal(rng: &mut Rng, secrets: &[String]) -> StubResponse {
    let body = body_with(rng, secrets, 2_000);
    let status = rng.pick(&[400, 401, 403, 500, 503]);
    match rng.below(5) {
        0 => StubResponse::new(status, body.into_bytes()),
        1 => StubResponse::json(
            &serde_json::json!({"error": {"code": "invalid_request", "message": body}}),
            status,
        ),
        2 => StubResponse::json(
            &serde_json::json!({"error": "invalid_grant", "error_description": body}),
            status,
        ),
        3 => StubResponse::json(
            &serde_json::json!({"error": {"code": body_with(rng, secrets, 20)}}),
            status,
        ),
        _ => StubResponse::json(
            &serde_json::json!({"error_code": body_with(rng, secrets, 20), "error_description": body}),
            status,
        ),
    }
}

/// The events `recorder` logged since `since` events.
fn event_leaks(recorder: &EventRecorder, since: usize, secrets: &[String]) -> Vec<String> {
    recorder.events()[since..]
        .iter()
        .flat_map(|event| leaks(&format!("{event:?}"), secrets, "an event"))
        .collect()
}

async fn endpoint_client_leaks(seed: u64) -> Vec<String> {
    let server = StubChatServer::start().await.unwrap();
    let endpoint = LlmEndpoint::new(server.base_url().clone(), "stub-model");
    let clock = ManualClock::new();
    let recorder = EventRecorder::new();
    let clients: Vec<(String, OpenAiCompatibleClient)> = (0..ENDPOINT_CLIENTS)
        .map(|index| {
            let key = secret(&mut Rng::seeded(seed + CASES + index));
            let client = OpenAiCompatibleClient::new(endpoint.clone(), Some(&key))
                .with_retry(two_attempts())
                .with_clock(clock.clone())
                .with_observer(recorder.observer());
            (key, client)
        })
        .collect();
    let mut found = Vec::new();
    for case in 0..CASES {
        let mut rng = Rng::seeded(seed + case);
        let (key, client) = &clients[usize::try_from(case % ENDPOINT_CLIENTS).unwrap()];
        let secrets = vec![key.clone()];
        let body = body_with(&mut rng, &secrets, 2_000);
        let status = error_status(&mut rng);
        server.respond(always(match rng.below(5) {
            0 => StubResponse::new(status, body.into_bytes()),
            1 => StubResponse::json(&serde_json::json!({"error": {"message": body}}), status),
            2 => StubResponse::new(200, body.into_bytes()),
            3 => scripts.refusal(&body),
            _ => StubResponse::json(
                &serde_json::json!({"error": {"message": "nope", "echo": body}}),
                status,
            ),
        }));
        let since = recorder.events().len();
        if let Err(error) = client.complete_llm(&text_request()).await {
            found.extend(error_leaks(&error, &secrets));
        }
        found.extend(event_leaks(&recorder, since, &secrets));
        found.extend(leaks(&format!("{client:?}"), &secrets, "the client"));
    }
    found
}

async fn codex_client_leaks(seed: u64) -> Vec<String> {
    let home = CodexHome::new().await;
    let backend = StubChatServer::start().await.unwrap();
    let mut endpoint = LlmEndpoint::codex("gpt-stub", 200_000);
    endpoint.base_url = backend.base_url().clone();
    let recorder = EventRecorder::new();
    let client = CodexResponsesClient::new(endpoint, Arc::new(home.store()))
        .with_retry(two_attempts())
        .with_clock(ManualClock::new())
        .with_observer(recorder.observer());
    let mut found = Vec::new();
    for case in 0..CASES {
        let mut rng = Rng::seeded(seed + case);
        let secrets = codex_secrets(&mut rng);
        home.write(signed_in(&secrets, false));
        let body = body_with(&mut rng, &secrets, 2_000);
        let status = error_status(&mut rng);
        let shape = rng.below(9);
        backend.respond(always(match shape {
            0 => StubResponse::new(status, body.into_bytes()),
            1 => StubResponse::json(
                &serde_json::json!({"error": {"message": body, "type": "invalid_request_error"}}),
                status,
            ),
            2 => StubResponse::json(&serde_json::json!({"detail": body}), status),
            3 => StubResponse::new(200, body.into_bytes())
                .with_header("Content-Type", "application/json"),
            4 => scripts.responses_error_event(&body, "server_error"),
            5 => scripts.responses_failed(&body),
            6 => scripts.responses_refusal(&body),
            7 => {
                home.server
                    .respond(always(refresh_refusal(&mut rng, &secrets)));
                StubResponse::new(401, b"{}".to_vec())
            }
            _ => StubResponse::new(rng.pick(&[200, 400, 500]), body.into_bytes()),
        }));
        let since = recorder.events().len();
        let result = if shape == 8 {
            client.list_models().await.map(|_| ())
        } else {
            client.complete_llm(&text_request()).await.map(|_| ())
        };
        if let Err(error) = result {
            found.extend(error_leaks(&error, &secrets));
            if let CodexError::Credential(error) = &error {
                found.extend(credential_leaks(error, &secrets));
            }
        }
        found.extend(event_leaks(&recorder, since, &secrets));
    }
    found
}

async fn token_refresh_leaks(seed: u64) -> Vec<String> {
    let home = CodexHome::new().await;
    let store = home.store();
    let mut found = Vec::new();
    for case in 0..CASES {
        let mut rng = Rng::seeded(seed + case);
        let secrets = codex_secrets(&mut rng);
        home.write(signed_in(&secrets, true));
        home.server
            .respond(always(refresh_refusal(&mut rng, &secrets)));
        match store.current().await {
            Err(error) => found.extend(credential_leaks(&error, &secrets)),
            Ok(_) => found.push(format!("case {case}: the refresh went through")),
        }
    }
    found
}

#[tokio::test]
async fn no_random_secret_echoed_anywhere_in_a_random_body_reaches_an_error_or_an_event() {
    let mut found = endpoint_client_leaks(1).await;
    found.extend(codex_client_leaks(100_000).await);
    found.extend(token_refresh_leaks(200_000).await);
    assert!(
        found.is_empty(),
        "{} leaks, the first: {:#?}",
        found.len(),
        &found[..found.len().min(3)]
    );
}
