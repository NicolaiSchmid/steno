//! Both clients against stub servers: the wire formats, auth, retries on
//! the manual clock, the structured output and parameter fallbacks, the
//! probe, the Codex backend's quirks and the 401 refresh.

#![allow(clippy::type_complexity)] // the case tables spell out their tuples

mod common;

use std::sync::Arc;

use common::*;
use steno_core::{LlmFinishReason, LlmMessage, LlmRequest, LlmResponseFormat, LlmRole, LlmUsage};
use steno_llm::testing::{STALL_DEADLINE, StubResponse, scripts};
use steno_llm::wire::{ChatErrorDetail, ChatErrorEnvelope, CodexModel, ResponsesInputItem};
use steno_llm::{
    CodexCredentialError, CodexError, CodexResponsesClient, LlmClient, LlmClientEvent, LlmEndpoint,
    LlmError, OpenAiCompatibleClient, RetryPolicy, StructuredOutputMode,
};

fn llm(error: CodexError) -> LlmError {
    match error {
        CodexError::Llm(error) => error,
        CodexError::Credential(error) => panic!("expected an LLM error, got {error}"),
    }
}

fn credential(error: CodexError) -> CodexCredentialError {
    match error {
        CodexError::Credential(error) => error,
        CodexError::Llm(error) => panic!("expected a credential error, got {error}"),
    }
}

#[allow(clippy::unnecessary_wraps)]
fn usage(prompt: i64, completion: i64) -> Option<LlmUsage> {
    Some(LlmUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        requests: 1,
    })
}

fn http(status: u16, body: &str) -> LlmError {
    LlmError::Http {
        status,
        body: body.to_owned(),
    }
}

#[tokio::test]
async fn sends_bearer_auth_and_the_chat_body_and_parses_the_reply() {
    let harness = ClientHarness::new().await;
    harness.server.enqueue([scripts.completion(
        "{\"hi\":true}",
        Some("stop"),
        usage(12, 4),
        "served-model",
    )]);
    let response = harness
        .client
        .complete_llm(&request("cleanup", schema_format("reply")))
        .await
        .unwrap();
    assert_eq!(response.text, "{\"hi\":true}");
    assert_eq!(response.finish_reason, LlmFinishReason::Stop);
    assert_eq!(response.usage, usage(12, 4));
    assert_eq!(response.model.as_deref(), Some("served-model"));

    let recorded = &harness.server.requests()[0];
    let header = |name: &str| recorded.headers.get(name).map(String::as_str);
    assert_eq!(recorded.path, "/v1/chat/completions");
    assert_eq!(recorded.authorization(), Some(bearer(API_KEY).as_str()));
    assert_eq!(header("x-steno-purpose"), Some("cleanup"));
    assert_eq!(header("content-type"), Some("application/json"));
    assert_eq!(header("user-agent"), Some(steno_llm::USER_AGENT));
    let chat = recorded.chat.as_ref().unwrap();
    assert_eq!(chat.model, "stub-model");
    let roles: Vec<&str> = chat.messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, ["system", "user"]);
    assert_eq!((chat.temperature, chat.max_tokens), (Some(0.0), Some(64)));
    let schema = chat
        .response_format
        .as_ref()
        .unwrap()
        .json_schema
        .as_ref()
        .unwrap();
    assert_eq!((schema.name.as_str(), schema.strict), ("reply", true));

    // No key, or an empty one, sends no `Authorization`; the endpoint's
    // ceiling stands in for a missing `max_tokens`.
    for key in [None, Some("")] {
        let harness =
            ClientHarness::build(RetryPolicy::NONE, key, |e| e.max_output_tokens = 777).await;
        harness.server.enqueue([scripts.text("ok")]);
        let mut request = text_request();
        request.max_tokens = None;
        harness.client.complete_llm(&request).await.unwrap();
        let recorded = &harness.server.requests()[0];
        assert_eq!(recorded.authorization(), None, "{key:?}");
        let chat = recorded.chat.as_ref().unwrap();
        assert_eq!(chat.max_tokens, Some(777));
        assert!(
            chat.response_format.is_none(),
            "a text request asks for none"
        );
    }
}

/// Answers that end the call at once: no retry, no sleep.
#[tokio::test]
async fn final_answers_are_returned_without_a_retry() {
    // A partial or null `usage` never fails a good answer.
    let partial = r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":null}}"#;
    let stop = LlmFinishReason::Stop;
    let cases: Vec<(
        StubResponse,
        Result<(&str, LlmFinishReason, Option<LlmUsage>), LlmError>,
    )> = vec![
        (
            scripts.truncated("{\"se"),
            Ok(("{\"se", LlmFinishReason::Length, usage(10, 5))),
        ),
        (
            scripts.raw_completion(partial),
            Ok(("ok", stop, usage(7, 0))),
        ),
        (
            scripts.refusal("I cannot help."),
            Err(LlmError::Refused("I cannot help.".to_owned())),
        ),
        (
            scripts.unauthorized(),
            Err(http(401, "Incorrect API key provided")),
        ),
        (
            scripts.bad_request("model `x` does not exist"),
            Err(http(400, "model `x` does not exist")),
        ),
        (
            StubResponse::new(404, b"<html>gone</html>".to_vec()),
            Err(http(404, "<html>gone</html>")),
        ),
    ];
    let harness = ClientHarness::new().await;
    for (count, (reply, expected)) in cases.into_iter().enumerate() {
        harness.server.enqueue([reply]);
        let outcome = harness
            .client
            .complete_llm(&json_request())
            .await
            .map(|r| (r.text, r.finish_reason, r.usage));
        let expected = expected.map(|(text, finish, usage)| (text.to_owned(), finish, usage));
        assert_eq!(outcome, expected, "case {count}");
        assert_eq!(harness.server.request_count(), count + 1);
    }
    assert_eq!(
        harness.log.retry_delays(),
        Vec::<std::time::Duration>::new()
    );
}

/// Transient failures back off on the clock: `Retry-After` when sent and
/// representable, else 2 s, 4 s, … up to the policy's ceiling.
#[tokio::test]
async fn transient_failures_back_off_on_the_clock() {
    let ok = || scripts.text("ok");
    let five_hundred = || scripts.server_error(500);
    let cases: Vec<(&str, Vec<StubResponse>, Result<(), LlmError>, Vec<u64>)> = vec![
        (
            "three 500s",
            vec![five_hundred(), five_hundred(), five_hundred()],
            Err(http(500, "The server had an error")),
            vec![2, 4],
        ),
        (
            "Retry-After",
            vec![scripts.rate_limited(Some("7")), ok()],
            Ok(()),
            vec![7],
        ),
        (
            "unrepresentable Retry-After",
            vec![
                scripts.rate_limited(Some("inf")),
                scripts.rate_limited(Some("1e300")),
                ok(),
            ],
            Ok(()),
            vec![2, 30],
        ),
        (
            "dropped connection",
            vec![StubResponse::drop_connection(), ok()],
            Ok(()),
            vec![2],
        ),
    ];
    for (name, replies, expected, delays) in cases {
        let harness = ClientHarness::new().await;
        let attempts = replies.len();
        harness.server.enqueue(replies);
        let driver = harness.drive_retries();
        let outcome = harness
            .client
            .complete_llm(&json_request())
            .await
            .map(|_| ());
        driver.abort();
        assert_eq!(outcome, expected, "{name}");
        assert_eq!(harness.server.request_count(), attempts, "{name}");
        let delays: Vec<_> = delays.into_iter().map(secs).collect();
        assert_eq!(harness.log.retry_delays(), delays, "{name}");
        assert_eq!(
            harness.clock.offset(),
            delays.iter().sum(),
            "{name}: slept on the clock"
        );
    }
    let harness = ClientHarness::build(RetryPolicy::NONE, Some(API_KEY), |_| {}).await;
    harness.server.enqueue([scripts.rate_limited(None)]);
    let error = harness
        .client
        .complete_llm(&json_request())
        .await
        .unwrap_err();
    assert_eq!(error, LlmError::RateLimited { retry_after: None });
    assert_eq!(harness.clock.pending_sleepers(), 0);
}

#[tokio::test]
async fn a_timeout_on_the_clock_cancels_the_attempt_and_retries() {
    let harness = ClientHarness::build(RetryPolicy::default(), Some(API_KEY), |e| {
        e.request_timeout = secs(30);
    })
    .await;
    harness
        .server
        .enqueue([StubResponse::hang(), scripts.text("late")]);
    let driver = harness.drive_retries();
    let client = OpenAiCompatibleClient::new(harness.endpoint.clone(), Some(API_KEY))
        .with_clock(harness.clock.clone())
        .with_observer(harness.log.observer());
    let task = tokio::spawn(async move { client.complete_llm(&json_request()).await });
    harness.server.received(1).await;
    assert!(harness.clock.wait_for_sleepers(1, STALL_DEADLINE).await);
    harness.clock.advance(secs(30));
    assert_eq!(task.await.unwrap().unwrap().text, "late");
    driver.abort();
    assert!(
        harness
            .log
            .events()
            .contains(&LlmClientEvent::TimedOut { attempt: 1 })
    );
    assert_eq!(harness.log.retry_delays(), [secs(2)]);
}

fn format_kinds(harness: &ClientHarness) -> Vec<Option<String>> {
    harness
        .server
        .requests()
        .iter()
        .map(|r| {
            r.chat
                .as_ref()?
                .response_format
                .as_ref()
                .map(|f| f.kind.clone())
        })
        .collect()
}

/// A 400 naming the structured output request steps the mode down without
/// using an attempt, and the client keeps the weaker mode.
#[tokio::test]
async fn a_rejected_response_format_steps_the_mode_down_and_is_remembered() {
    let schema = || request("test", schema_format("reply"));
    let kind = |k: &str| Some(k.to_owned());
    let cases: Vec<(
        &[&str],
        LlmRequest,
        Vec<Option<String>>,
        StructuredOutputMode,
    )> = vec![
        (
            &["json_schema"],
            schema(),
            vec![kind("json_schema"), kind("json_object")],
            StructuredOutputMode::JsonObject,
        ),
        (
            &["json_schema", "json_object"],
            schema(),
            vec![kind("json_schema"), kind("json_object"), None],
            StructuredOutputMode::PromptOnly,
        ),
        // A JSON object request walks the chain mode by mode.
        (
            &["json_object"],
            json_request(),
            vec![kind("json_object"), kind("json_object"), None],
            StructuredOutputMode::PromptOnly,
        ),
    ];
    for (rejecting, request, kinds, mode) in cases {
        let harness = ClientHarness::new().await;
        harness.server.respond(scripts.format_rejecting_server(
            &["stub-model"],
            rejecting,
            scripts.text("{}"),
        ));
        harness.client.complete_llm(&request).await.unwrap();
        assert_eq!(format_kinds(&harness), kinds, "{rejecting:?}");
        assert_eq!(harness.client.resolved_mode(), mode);
        assert!(
            harness
                .log
                .events()
                .contains(&LlmClientEvent::ModeDowngraded { to: mode })
        );
        harness.client.complete_llm(&schema()).await.unwrap();
        assert_eq!(format_kinds(&harness).last(), kinds.last(), "remembered");
        assert_eq!(harness.clock.pending_sleepers(), 0, "a resend never sleeps");
    }

    // Not an attempt: one downgrade and one 500 fit a two-attempt policy.
    let harness =
        ClientHarness::build(RetryPolicy::with_max_attempts(2), Some(API_KEY), |_| {}).await;
    harness.server.enqueue([
        scripts.rejects_response_format(),
        scripts.server_error(500),
        scripts.text("{}"),
    ]);
    let driver = harness.drive_retries();
    harness.client.complete_llm(&schema()).await.unwrap();
    driver.abort();

    // A 400 that names something else, a text request, or one already at
    // `PromptOnly` is a plain error.
    let cases = [
        (
            StructuredOutputMode::JsonSchema,
            scripts.bad_request("model `stub-model` does not exist"),
            schema(),
        ),
        (
            StructuredOutputMode::JsonSchema,
            scripts.rejects_response_format(),
            text_request(),
        ),
        (
            StructuredOutputMode::PromptOnly,
            scripts.rejects_response_format(),
            schema(),
        ),
    ];
    for (start, reply, request) in cases {
        let harness = ClientHarness::build(RetryPolicy::default(), Some(API_KEY), |e| {
            e.structured_output_mode = start;
        })
        .await;
        harness.server.enqueue([reply]);
        let error = harness.client.complete_llm(&request).await.unwrap_err();
        assert!(
            matches!(error, LlmError::Http { status: 400, .. }),
            "{error}"
        );
        assert_eq!(harness.server.request_count(), 1);
        assert_eq!(harness.client.resolved_mode(), start);
    }
}

/// The cleanup pass sends two chunks at once, so both can meet the first
/// 400: the mode steps down once per step and never back up.
#[tokio::test]
async fn concurrent_rejections_step_the_mode_down_once_per_step() {
    let harness = ClientHarness::new().await;
    harness.server.respond(scripts.format_rejecting_server(
        &["stub-model"],
        &["json_schema", "json_object"],
        scripts.text("{}"),
    ));
    harness.server.hold_responses();
    let (first, second) = (
        request("a", schema_format("r")),
        request("b", schema_format("r")),
    );
    let (a, b, ()) = tokio::join!(
        harness.client.complete_llm(&first),
        harness.client.complete_llm(&second),
        async {
            harness.server.received(2).await;
            harness.server.release();
        }
    );
    a.unwrap();
    b.unwrap();
    let downgrades: Vec<_> = harness
        .log
        .events()
        .into_iter()
        .filter_map(|event| match event {
            LlmClientEvent::ModeDowngraded { to } => Some(to),
            _ => None,
        })
        .collect();
    assert_eq!(
        downgrades,
        [
            StructuredOutputMode::JsonObject,
            StructuredOutputMode::PromptOnly
        ]
    );
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::PromptOnly
    );
    assert_eq!(harness.server.request_count(), 6);
}

/// Reasoning models reject `max_tokens` and `temperature` by name: the
/// request is resent as `max_completion_tokens` and without a temperature,
/// once per parameter, and the client keeps that spelling.
#[tokio::test]
async fn rejected_parameters_are_respelled_and_remembered() {
    let harness = ClientHarness::new().await;
    harness
        .server
        .respond(scripts.reasoning_model(scripts.text("{}")));
    harness
        .client
        .complete_llm(&request("test", schema_format("reply")))
        .await
        .unwrap();
    harness.client.complete_llm(&text_request()).await.unwrap();
    let wire: Vec<_> = harness
        .server
        .requests()
        .into_iter()
        .filter_map(|r| r.chat)
        .collect();
    let spelled: Vec<_> = wire
        .iter()
        .map(|c| (c.max_tokens, c.max_completion_tokens, c.temperature))
        .collect();
    assert_eq!(
        spelled,
        [
            (Some(64), None, Some(0.0)),
            (None, Some(64), Some(0.0)),
            (None, Some(64), None),
            (None, Some(64), None),
        ]
    );
    let events = harness.log.events();
    assert!(events.contains(&LlmClientEvent::ParameterRejected("max_tokens".to_owned())));
    assert!(events.contains(&LlmClientEvent::ParameterRejected("temperature".to_owned())));
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::JsonSchema
    );
    assert_eq!(harness.clock.pending_sleepers(), 0);

    // The same rejection again, or one naming another field, is the answer.
    for (param, requests) in [("max_tokens", 2), ("messages", 1)] {
        let harness = ClientHarness::build(RetryPolicy::NONE, Some(API_KEY), |_| {}).await;
        harness
            .server
            .respond(Arc::new(move |_| Some(scripts.rejects_parameter(param))));
        let error = harness
            .client
            .complete_llm(&text_request())
            .await
            .unwrap_err();
        assert!(
            matches!(error, LlmError::Http { status: 400, .. }),
            "{error}"
        );
        assert_eq!(harness.server.request_count(), requests, "{param}");
    }
}

#[tokio::test]
async fn the_probe_reports_the_model_list_the_mode_and_failures() {
    let harness = ClientHarness::new().await;
    harness.server.respond(scripts.format_rejecting_server(
        &["other"],
        &["json_schema"],
        scripts.text("{\"ok\":true}"),
    ));
    let probe = harness.client.probe_llm().await.unwrap();
    assert_eq!(probe.model_listed, Some(false));
    assert_eq!(probe.resolved_mode, StructuredOutputMode::JsonObject);
    assert_eq!(probe.account_line, None);
    let paths: Vec<String> = harness
        .server
        .requests()
        .iter()
        .map(|r| format!("{} {} {:?}", r.method, r.path, r.purpose))
        .collect();
    assert_eq!(
        paths,
        [
            "GET /v1/models Some(\"probe\")",
            "POST /v1/chat/completions Some(\"probe\")",
            "POST /v1/chat/completions Some(\"probe\")",
        ]
    );

    // A server without a model list is fine; a failing completion is not.
    let harness = ClientHarness::build(RetryPolicy::NONE, Some(API_KEY), |_| {}).await;
    harness.server.respond(Arc::new(|request| {
        Some(if request.method == "GET" {
            scripts.bad_request("no such route")
        } else {
            scripts.text("{\"ok\":true}")
        })
    }));
    assert_eq!(harness.client.probe_llm().await.unwrap().model_listed, None);
    harness.server.respond(Arc::new(|request| {
        Some(if request.method == "GET" {
            scripts.models(&["stub-model"])
        } else {
            scripts.unauthorized()
        })
    }));
    assert_eq!(
        harness.client.probe_llm().await.unwrap_err(),
        http(401, "Incorrect API key provided")
    );
    // Port 1 is closed on every runner.
    let endpoint = LlmEndpoint::new(url::Url::parse("http://127.0.0.1:1/v1").unwrap(), "m");
    let client = OpenAiCompatibleClient::new(endpoint, None).with_retry(RetryPolicy::NONE);
    let error = client.probe_llm().await.unwrap_err();
    assert!(matches!(error, LlmError::Transport(_)), "{error}");
}

/// Pinned against the real Codex backend: it rejects
/// `max_output_tokens` and `temperature`, and takes `originator: steno`.
#[tokio::test]
async fn codex_sends_the_sign_in_and_a_responses_body_and_parses_the_stream() {
    let harness = CodexHarness::new().await;
    harness.backend.enqueue([scripts.responses_stream(
        "{\"hi\":true}",
        "completed",
        None,
        usage(12, 4),
        "gpt-served",
    )]);
    let response = harness
        .client
        .complete_llm(&request("cleanup", schema_format("reply")))
        .await
        .unwrap();
    assert_eq!(response.text, "{\"hi\":true}");
    assert_eq!(response.usage, usage(12, 4));
    assert_eq!(response.model.as_deref(), Some("gpt-served"));

    let recorded = &harness.backend.requests()[0];
    let header = |name: &str| recorded.headers.get(name).map(String::as_str);
    assert_eq!(recorded.path, "/v1/responses");
    assert_eq!(
        recorded.authorization(),
        Some(bearer(&CodexHome::access_token(3_600, "plus")).as_str())
    );
    assert_eq!(header("chatgpt-account-id"), Some("acct_stored"));
    assert_eq!(header("originator"), Some("steno"));
    assert_eq!(header("user-agent"), Some(steno_llm::USER_AGENT));
    assert_eq!(header("accept"), Some("text/event-stream"));
    assert!(header("session-id").is_some_and(|s| !s.is_empty()));
    let body = recorded.responses.as_ref().unwrap();
    assert_eq!(body.model, "gpt-stub");
    assert_eq!(body.instructions.as_deref(), Some("You are a test."));
    assert_eq!(body.input[0].content[0].kind, "input_text");
    assert!(body.stream && !body.store);
    assert_eq!(body.reasoning.as_ref().unwrap().effort, "low");
    let format = &body.text.as_ref().unwrap().format;
    assert_eq!(
        (format.kind.as_str(), format.strict),
        ("json_schema", Some(true))
    );
    let raw = recorded.body_text();
    for rejected in ["max_output_tokens", "max_tokens", "temperature"] {
        assert!(!raw.contains(rejected), "{rejected} in {raw}");
    }

    // System prompts join as instructions, a repair round's answer goes
    // back as `output_text`, and the summary reasons harder.
    let message = |role, content: &str| LlmMessage {
        role,
        content: content.to_owned(),
    };
    let body = CodexResponsesClient::request_body(
        &LlmRequest {
            messages: vec![
                message(LlmRole::System, "A"),
                message(LlmRole::System, "B"),
                message(LlmRole::User, "Q"),
                message(LlmRole::Assistant, "bad json"),
            ],
            response_format: LlmResponseFormat::Text,
            temperature: Some(0.2),
            max_tokens: Some(100),
            purpose: "summary".to_owned(),
        },
        "m",
        StructuredOutputMode::JsonSchema,
    );
    assert_eq!(body.instructions.as_deref(), Some("A\n\nB"));
    assert_eq!(
        body.input[1],
        ResponsesInputItem::from(&message(LlmRole::Assistant, "bad json"))
    );
    assert_eq!(body.input[1].content[0].kind, "output_text");
    assert_eq!(body.reasoning.unwrap().effort, "medium");
    assert_eq!(body.text, None);

    // The backend's own rejection of the field surfaces as it is.
    harness
        .backend
        .enqueue([scripts.codex_rejects_parameter("max_output_tokens")]);
    assert_eq!(
        llm(harness
            .client
            .complete_llm(&text_request())
            .await
            .unwrap_err()),
        http(400, "Unsupported parameter: max_output_tokens")
    );
    // The backend names `text.format` where chat servers name
    // `response_format`; either starts the same mode fallback.
    let schema = request("test", schema_format("r"));
    harness.backend.enqueue([
        scripts.bad_request("text.format is not supported"),
        scripts.stream("{}"),
    ]);
    harness.client.complete_llm(&schema).await.unwrap();
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::JsonObject
    );
}

/// The backend hides models newer than `client_version`, so the list asks
/// with a high one.
#[tokio::test]
async fn codex_lists_models_with_the_client_version_filter_and_probes() {
    let harness = CodexHarness::with_retry(RetryPolicy::NONE).await;
    harness.backend.respond(Arc::new(|request| {
        Some(if request.method == "GET" {
            scripts.codex_models(vec![
                CodexModel::new("gpt-a", "A"),
                CodexModel {
                    visibility: Some("hide".to_owned()),
                    ..CodexModel::new("gpt-stub", "B")
                },
            ])
        } else {
            scripts.stream("{\"ok\":true}")
        })
    }));
    let models = harness.client.list_models().await.unwrap();
    let listed: Vec<(&str, bool)> = models
        .iter()
        .map(|m| (m.slug.as_str(), m.is_listed()))
        .collect();
    assert_eq!(listed, [("gpt-a", true), ("gpt-stub", false)]);
    let recorded = &harness.backend.requests()[0];
    assert_eq!(recorded.path, "/v1/models?client_version=99.0.0");
    assert_eq!(
        recorded.headers.get("originator").map(String::as_str),
        Some("steno")
    );
    assert_eq!(
        recorded
            .headers
            .get("chatgpt-account-id")
            .map(String::as_str),
        Some("acct_stored")
    );
    let probe = harness.client.probe_llm().await.unwrap();
    assert_eq!(probe.model_listed, Some(true));
    assert_eq!(
        probe.account_line.as_deref(),
        Some("nicolai@example.com (Plus)")
    );

    // A failed model list is tolerated; no sign-in sends nothing.
    harness.backend.respond(Arc::new(|request| {
        Some(if request.method == "GET" {
            scripts.unauthorized()
        } else {
            scripts.stream("{\"ok\":true}")
        })
    }));
    assert_eq!(harness.client.probe_llm().await.unwrap().model_listed, None);
    std::fs::remove_file(harness.home.file()).unwrap();
    let sent = harness.backend.request_count();
    assert_eq!(
        credential(harness.client.probe_llm().await.unwrap_err()),
        CodexCredentialError::NotSignedIn
    );
    let boxed = steno_core::LanguageModel::complete(&harness.client, &json_request())
        .await
        .unwrap_err();
    assert_eq!(
        downcast::<CodexCredentialError>(&boxed),
        CodexCredentialError::NotSignedIn
    );
    assert_eq!(harness.backend.request_count(), sent);
}

/// A 401 re-reads a file the CLI rotated, else refreshes once; a second
/// 401 is the answer.
#[tokio::test]
async fn codex_answers_a_401_with_one_refresh() {
    let stored = bearer(&CodexHome::access_token(3_600, "plus"));
    let fresh = CodexHome::access_token(3_600, "pro");

    let harness = CodexHarness::new().await;
    harness
        .home
        .server
        .enqueue([scripts.token_refresh(&fresh, Some("rt_2"), None)]);
    harness
        .backend
        .enqueue([scripts.unauthorized(), scripts.stream("ok")]);
    assert_eq!(
        harness
            .client
            .complete_llm(&text_request())
            .await
            .unwrap()
            .text,
        "ok"
    );
    assert_eq!(harness.authorizations(), [stored.clone(), bearer(&fresh)]);
    assert_eq!(harness.home.posted_refresh_tokens(), ["rt_original"]);
    assert!(
        harness.log.retry_delays().is_empty(),
        "a refresh is no retry"
    );

    let harness = CodexHarness::new().await;
    harness
        .home
        .server
        .enqueue([scripts.token_refresh(&fresh, Some("rt_2"), None)]);
    harness
        .backend
        .enqueue([scripts.unauthorized(), scripts.unauthorized()]);
    let error = llm(harness
        .client
        .complete_llm(&text_request())
        .await
        .unwrap_err());
    assert!(
        matches!(error, LlmError::Http { status: 401, .. }),
        "{error}"
    );
    assert_eq!(harness.backend.request_count(), 2);

    // The CLI rotates the file while the first request is in flight.
    let harness = CodexHarness::new().await;
    let file = harness.home.file();
    let rotated = fresh.clone();
    harness.backend.respond(Arc::new(move |request| {
        if request.index == 0 {
            write_auth(
                &file,
                AuthFile::default().access(&rotated).refresh("rt_cli"),
            );
            return Some(scripts.unauthorized());
        }
        Some(scripts.stream("ok"))
    }));
    harness.client.complete_llm(&text_request()).await.unwrap();
    assert_eq!(harness.authorizations(), [stored, bearer(&fresh)]);
    assert_eq!(harness.home.server.request_count(), 0);
}

/// A plan limit is final whatever its spelling; overloads, 5xx and a token
/// endpoint hiccup back off like a dropped connection.
#[tokio::test]
async fn codex_plan_limits_are_final_and_transient_failures_retry() {
    let plan_limit = |message: &str| format!("ChatGPT plan limit reached: {message}");
    let not_included = StubResponse::json(
        &ChatErrorEnvelope {
            error: ChatErrorDetail {
                message: "Not included in your plan.".to_owned(),
                kind: Some("usage_not_included".to_owned()),
                ..ChatErrorDetail::default()
            },
        },
        403,
    );
    let finals = [
        (
            scripts.codex_usage_limit(),
            http(429, &plan_limit("You have hit your usage limit.")),
        ),
        (
            not_included,
            http(403, &plan_limit("Not included in your plan.")),
        ),
        (
            scripts.responses_error_event("No plan.", "usage_not_included"),
            http(400, &plan_limit("No plan.")),
        ),
        (
            scripts.bad_request("The model does not exist"),
            http(400, "The model does not exist"),
        ),
    ];
    for (reply, expected) in finals {
        let harness = CodexHarness::new().await;
        harness.backend.enqueue([reply]);
        let error = llm(harness
            .client
            .complete_llm(&text_request())
            .await
            .unwrap_err());
        assert_eq!(error, expected);
        assert_eq!(harness.backend.request_count(), 1);
        assert_eq!(
            harness.clock.pending_sleepers(),
            0,
            "no Retry-After is waited on"
        );
    }

    let transient = [
        vec![scripts.server_error(503), scripts.server_error(408)],
        vec![scripts.rate_limited(Some("3"))],
        vec![scripts.responses_error_event("busy", "server_is_overloaded")],
        vec![StubResponse::drop_connection()],
    ];
    for replies in transient {
        let harness = CodexHarness::new().await;
        let retries = replies.len();
        harness.backend.enqueue(replies);
        harness.backend.enqueue([scripts.stream("ok")]);
        let driver = harness.drive_retries();
        assert_eq!(
            harness
                .client
                .complete_llm(&text_request())
                .await
                .unwrap()
                .text,
            "ok"
        );
        driver.abort();
        assert_eq!(harness.log.retry_delays().len(), retries);
        assert_eq!(harness.home.server.request_count(), 0, "no sign-in problem");
    }

    let harness = CodexHarness::new().await;
    harness.home.write_expiring();
    let fresh = CodexHome::access_token(3_600, "plus");
    harness.home.server.enqueue([
        scripts.server_error(503),
        scripts.token_refresh(&fresh, Some("rt_2"), None),
    ]);
    harness.backend.enqueue([scripts.stream("ok")]);
    let driver = harness.drive_retries();
    harness.client.complete_llm(&text_request()).await.unwrap();
    driver.abort();
    assert_eq!(harness.authorizations(), [bearer(&fresh)]);
    assert_eq!(harness.log.retry_delays(), [secs(2)]);
}

/// Two chunks on an expiring sign-in at once spend the refresh token once;
/// a second POST of it would be refused as reused and sign the CLI out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_completions_on_an_expiring_sign_in_share_one_refresh() {
    let harness = CodexHarness::with_retry(RetryPolicy::NONE).await;
    harness.home.write_expiring();
    let fresh = CodexHome::access_token(3_600, "plus");
    let reply = fresh.clone();
    harness.home.server.respond(Arc::new(move |request| {
        Some(if request.index == 0 {
            scripts.token_refresh(&reply, Some("rt_2"), None)
        } else {
            scripts.token_refresh_rejected("refresh_token_reused", 400)
        })
    }));
    harness
        .backend
        .respond(Arc::new(|_| Some(scripts.stream("ok"))));
    let request = text_request();
    let results =
        futures_util::future::join_all((0..16).map(|_| harness.client.complete_llm(&request)))
            .await;
    assert!(results.into_iter().all(|r| r.unwrap().text == "ok"));
    assert_eq!(harness.home.posted_refresh_tokens(), ["rt_original"]);
    assert!(
        harness
            .authorizations()
            .iter()
            .all(|a| *a == bearer(&fresh))
    );
}

/// Without trusted root certificates the clients are made without a panic
/// and every call fails with the reason. Reruns itself with
/// `SSL_CERT_FILE` and `SSL_CERT_DIR` pointing at an empty file and folder.
#[cfg(all(unix, not(target_vendor = "apple")))]
#[tokio::test]
async fn without_root_certificates_the_clients_fail_their_calls_instead_of_panicking() {
    const CHILD: &str = "STENO_LLM_TEST_NO_CERTIFICATES";
    if std::env::var_os(CHILD).is_none() {
        let roots = tempfile::tempdir().unwrap();
        let empty = roots.path().join("empty.pem");
        std::fs::write(&empty, b"").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "without_root_certificates_the_clients_fail_their_calls_instead_of_panicking",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env("SSL_CERT_FILE", &empty)
            .env("SSL_CERT_DIR", roots.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("no-certificates child passed"), "{stdout}");
        return;
    }
    let expected = LlmError::HttpClientUnavailable(
        "no trusted root certificates were found on this computer".into(),
    );
    assert_eq!(
        steno_llm::transport::default_http_client().unwrap_err(),
        expected
    );
    let endpoint = LlmEndpoint::new(url::Url::parse("http://127.0.0.1:9/v1").unwrap(), "m");
    let client = OpenAiCompatibleClient::new(endpoint, None);
    assert_eq!(
        client.complete_llm(&text_request()).await.unwrap_err(),
        expected
    );
    assert_eq!(client.probe_llm().await.unwrap_err(), expected);
    let home = tempfile::tempdir().unwrap();
    let store = Arc::new(steno_llm::CodexCredentialStore::new(home.path()));
    let _codex = CodexResponsesClient::new(LlmEndpoint::codex("m", 32_000), store);
    println!("no-certificates child passed");
}
