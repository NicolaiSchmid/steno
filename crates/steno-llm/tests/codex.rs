//! The Codex client against a stub backend and a temporary Codex home: the
//! Responses wire format and the backend's quirks (no `max_output_tokens`
//! or `temperature`, `client_version` on the model list, `originator`), the
//! 401 refresh, the plan limit, streams and redaction.
//! Swift: `CodexClientTests`, `CodexClientReviewTests`.

mod common;

use std::sync::Arc;

use common::*;
use steno_core::{
    LlmFinishReason, LlmMessage, LlmRequest, LlmResponseFormat, LlmRole, LlmUsage, Settings,
};
use steno_llm::testing::{StubResponse, scripts};
use steno_llm::wire::{
    ChatErrorDetail, ChatErrorEnvelope, CodexModel, ResponsesFormat, ResponsesIncompleteDetails,
    ResponsesInputItem, ResponsesOutputItem, ResponsesResponse, ResponsesUsage,
};
use steno_llm::{
    CodexCredentialError, CodexError, CodexResponsesClient, LlmClient, LlmClientEvent, LlmEndpoint,
    LlmError, RetryPolicy, StructuredOutputMode,
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

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

#[tokio::test]
async fn sends_the_sign_in_and_a_responses_body_and_parses_the_stream() {
    let harness = CodexHarness::new().await;
    harness.backend.enqueue([scripts.responses_stream(
        "{\"hi\":true}",
        "completed",
        None,
        Some(LlmUsage {
            prompt_tokens: 12,
            completion_tokens: 4,
            requests: 1,
        }),
        "gpt-served",
    )]);
    let response = harness
        .client
        .complete_llm(&request("cleanup", schema_format("reply")))
        .await
        .unwrap();
    assert_eq!(response.text, "{\"hi\":true}");
    assert_eq!(response.finish_reason, LlmFinishReason::Stop);
    assert_eq!(
        response.usage,
        Some(LlmUsage {
            prompt_tokens: 12,
            completion_tokens: 4,
            requests: 1
        })
    );
    assert_eq!(response.model.as_deref(), Some("gpt-served"));

    let recorded = &harness.backend.requests()[0];
    assert_eq!(recorded.path, "/v1/responses");
    assert_eq!(
        recorded.authorization().map(str::to_owned),
        Some(bearer(&CodexHome::access_token(3_600, "plus")))
    );
    let header = |name: &str| recorded.headers.get(name).map(String::as_str);
    assert_eq!(header("chatgpt-account-id"), Some("acct_stored"));
    assert_eq!(header("originator"), Some("steno"));
    assert_eq!(header("user-agent"), Some(steno_llm::USER_AGENT));
    assert_eq!(header("accept"), Some("text/event-stream"));
    assert!(header("session-id").is_some_and(|s| !s.is_empty()));
    assert_eq!(recorded.purpose.as_deref(), Some("cleanup"));
    assert!(recorded.chat.is_none(), "not a chat completion");
    let body = recorded.responses.as_ref().unwrap();
    assert_eq!(body.model, "gpt-stub");
    assert_eq!(body.instructions.as_deref(), Some("You are a test."));
    assert_eq!(
        body.input,
        [ResponsesInputItem::from(&LlmMessage {
            role: LlmRole::User,
            content: "Say hi as JSON.".to_owned()
        })]
    );
    assert_eq!(body.input[0].content[0].kind, "input_text");
    assert!(body.stream);
    assert!(!body.store);
    assert_eq!(body.reasoning.as_ref().unwrap().effort, "low");
    let format = &body.text.as_ref().unwrap().format;
    assert_eq!(format.kind, "json_schema");
    assert_eq!(format.name.as_deref(), Some("reply"));
    assert_eq!(format.strict, Some(true));
    // Neither field the backend rejects appears, under any spelling.
    let raw = recorded.body_text();
    assert!(!raw.contains("max_output_tokens"));
    assert!(!raw.contains("max_tokens"));
    assert!(!raw.contains("temperature"));
}

#[test]
fn summary_purposes_use_medium_effort_and_assistant_turns_are_output_text() {
    assert_eq!(CodexResponsesClient::reasoning_effort("summary"), "medium");
    assert_eq!(
        CodexResponsesClient::reasoning_effort("summary-map"),
        "medium"
    );
    assert_eq!(CodexResponsesClient::reasoning_effort("cleanup"), "low");
    assert_eq!(CodexResponsesClient::reasoning_effort("probe"), "low");
    let message = |role: LlmRole, content: &str| LlmMessage {
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
                message(LlmRole::User, "fix it"),
            ],
            response_format: LlmResponseFormat::JsonObject,
            temperature: None,
            max_tokens: None,
            purpose: "summary".to_owned(),
        },
        "m",
        StructuredOutputMode::JsonSchema,
    );
    assert_eq!(body.instructions.as_deref(), Some("A\n\nB"));
    let roles: Vec<&str> = body.input.iter().map(|i| i.role.as_str()).collect();
    assert_eq!(roles, ["user", "assistant", "user"]);
    assert_eq!(body.input[1].content[0].kind, "output_text");
    assert_eq!(body.text.unwrap().format, ResponsesFormat::json_object());
    let plain = CodexResponsesClient::request_body(
        &LlmRequest {
            messages: vec![message(LlmRole::User, "Q")],
            response_format: LlmResponseFormat::Text,
            temperature: None,
            max_tokens: None,
            purpose: "x".to_owned(),
        },
        "m",
        StructuredOutputMode::JsonSchema,
    );
    assert_eq!(plain.instructions, None);
    assert_eq!(plain.text, None);
}

#[tokio::test]
async fn a_stale_token_is_refreshed_before_the_first_request() {
    let harness = CodexHarness::new().await;
    harness
        .home
        .write(AuthFile::default().access(&CodexHome::access_token(60, "plus")));
    let fresh = CodexHome::access_token(3_600, "plus");
    harness
        .home
        .server
        .enqueue([scripts.token_refresh(&fresh, Some("rt_2"), None)]);
    harness.backend.enqueue([scripts.stream("ok")]);
    harness.client.complete_llm(&text_request()).await.unwrap();
    assert_eq!(
        harness.backend.requests()[0]
            .authorization()
            .map(str::to_owned),
        Some(bearer(&fresh))
    );
    assert_eq!(harness.home.server.request_count(), 1);
}

#[tokio::test]
async fn unauthorized_refreshes_the_sign_in_once_then_stands() {
    let harness = CodexHarness::new().await;
    let fresh = CodexHome::access_token(3_600, "pro");
    harness
        .home
        .server
        .enqueue([scripts.token_refresh(&fresh, Some("rt_2"), None)]);
    harness
        .backend
        .enqueue([scripts.unauthorized(), scripts.stream("ok")]);
    let response = harness.client.complete_llm(&text_request()).await.unwrap();
    assert_eq!(response.text, "ok");
    let auths: Vec<Option<String>> = harness
        .backend
        .requests()
        .iter()
        .map(|r| r.authorization().map(str::to_owned))
        .collect();
    assert_eq!(
        auths,
        [
            Some(bearer(&CodexHome::access_token(3_600, "plus"))),
            Some(bearer(&fresh))
        ]
    );

    // A second 401 after the refresh is the answer, not a loop.
    let second = CodexHarness::new().await;
    second
        .home
        .server
        .enqueue([scripts.token_refresh(&fresh, Some("rt_2"), None)]);
    second
        .backend
        .enqueue([scripts.unauthorized(), scripts.unauthorized()]);
    let error = llm(second
        .client
        .complete_llm(&text_request())
        .await
        .unwrap_err());
    assert!(
        matches!(error, LlmError::Http { status: 401, .. }),
        "{error}"
    );
    assert_eq!(second.backend.request_count(), 2);
    assert!(!second.events().iter().any(is_retrying));
}

#[tokio::test]
async fn no_sign_in_fails_before_any_request() {
    let harness = CodexHarness::new().await;
    std::fs::remove_file(harness.home.file()).unwrap();
    let error = credential(
        harness
            .client
            .complete_llm(&json_request())
            .await
            .unwrap_err(),
    );
    assert_eq!(error, CodexCredentialError::NotSignedIn);
    assert_eq!(harness.backend.requests().len(), 0);
    // Through the trait, the same error downcasts from the boxed form.
    let boxed = steno_core::LanguageModel::complete(&harness.client, &json_request())
        .await
        .unwrap_err();
    assert_eq!(
        downcast::<CodexCredentialError>(&boxed),
        CodexCredentialError::NotSignedIn
    );
}

#[tokio::test]
async fn plan_limit_is_not_retried_and_names_the_plan() {
    let harness = CodexHarness::new().await;
    harness.backend.enqueue([scripts.codex_usage_limit()]);
    let error = llm(harness
        .client
        .complete_llm(&json_request())
        .await
        .unwrap_err());
    match &error {
        LlmError::Http { status: 429, body } => {
            assert!(body.starts_with("ChatGPT plan limit reached"), "{body}");
        }
        other => panic!("expected a plan-limit http error, got {other}"),
    }
    assert!(!error.is_retryable());
    assert_eq!(harness.backend.request_count(), 1);
    assert_eq!(
        harness.clock.pending_sleepers(),
        0,
        "the Retry-After of an hour is not waited on"
    );
    assert!(!harness.events().iter().any(is_retrying));
    // The other spelling of the plan limit, on a 403, is the same.
    harness.backend.enqueue([StubResponse::json(
        &ChatErrorEnvelope {
            error: ChatErrorDetail {
                message: "Not included in your plan.".to_owned(),
                kind: Some("usage_not_included".to_owned()),
                ..ChatErrorDetail::default()
            },
        },
        403,
    )]);
    let not_included = llm(harness
        .client
        .complete_llm(&json_request())
        .await
        .unwrap_err());
    assert_eq!(
        not_included,
        LlmError::Http {
            status: 403,
            body: "ChatGPT plan limit reached: Not included in your plan.".to_owned()
        }
    );
}

#[tokio::test]
async fn an_ordinary_rate_limit_backs_off() {
    let harness = CodexHarness::new().await;
    harness
        .backend
        .enqueue([scripts.rate_limited(Some("3")), scripts.stream("ok")]);
    let driver = harness.drive_retries();
    let response = harness.client.complete_llm(&text_request()).await.unwrap();
    driver.abort();
    assert_eq!(response.text, "ok");
    assert_eq!(harness.backend.request_count(), 2);
    assert!(harness.events().contains(&LlmClientEvent::Retrying {
        after: secs(3),
        attempt: 1,
        reason: LlmError::RateLimited {
            retry_after: Some(secs(3))
        }
    }));
}

fn text_format_kinds(harness: &CodexHarness) -> Vec<Option<String>> {
    harness
        .backend
        .requests()
        .iter()
        .map(|r| {
            r.responses
                .as_ref()
                .and_then(|b| b.text.as_ref())
                .map(|t| t.format.kind.clone())
        })
        .collect()
}

#[tokio::test]
async fn a_rejected_schema_downgrades_the_mode_and_unsupported_parameters_surface() {
    let harness = CodexHarness::new().await;
    harness.backend.respond(Arc::new(|request| {
        let schema = request
            .responses
            .as_ref()
            .and_then(|b| b.text.as_ref())
            .is_some_and(|t| t.format.kind == "json_schema");
        Some(if schema {
            scripts.bad_request("Invalid value for text.format: json_schema is not supported")
        } else {
            scripts.stream("{}")
        })
    }));
    harness
        .client
        .complete_llm(&request("test", schema_format("r")))
        .await
        .unwrap();
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::JsonObject
    );
    assert_eq!(
        text_format_kinds(&harness),
        [
            Some("json_schema".to_owned()),
            Some("json_object".to_owned())
        ]
    );

    let second = CodexHarness::new().await;
    second
        .backend
        .enqueue([scripts.codex_unsupported_parameter("max_output_tokens")]);
    let error = llm(second
        .client
        .complete_llm(&text_request())
        .await
        .unwrap_err());
    assert_eq!(
        error,
        LlmError::Http {
            status: 400,
            body: "Unsupported parameter: max_output_tokens".to_owned()
        }
    );
}

#[tokio::test]
async fn secrets_never_appear_in_errors() {
    let harness = CodexHarness::new().await;
    let token = CodexHome::access_token(3_600, "plus");
    harness.backend.enqueue([scripts.bad_request(&format!(
        "bad token {token} for account acct_stored with refresh rt_original"
    ))]);
    let error = llm(harness
        .client
        .complete_llm(&text_request())
        .await
        .unwrap_err());
    let text = error.to_string();
    assert!(!text.contains(&token));
    assert!(!text.contains("acct_stored"));
    assert!(!text.contains("rt_original"));
    assert!(text.contains("[redacted]"));
}

#[tokio::test]
async fn list_models_probe_and_timeout() {
    let harness = CodexHarness::build(RetryPolicy::NONE, "gpt-b", |_| {}).await;
    harness.backend.respond(Arc::new(|request| {
        Some(
            if request.method == "GET" && request.path.starts_with("/v1/models") {
                scripts.codex_models(vec![
                    CodexModel {
                        context_window: Some(272_000),
                        ..CodexModel::new("gpt-a", "A")
                    },
                    CodexModel {
                        visibility: Some("hide".to_owned()),
                        ..CodexModel::new("gpt-b", "B")
                    },
                ])
            } else {
                scripts.stream("{\"ok\":true}")
            },
        )
    }));
    let models = harness.client.list_models().await.unwrap();
    let slugs: Vec<&str> = models.iter().map(|m| m.slug.as_str()).collect();
    assert_eq!(slugs, ["gpt-a", "gpt-b"]);
    let listed: Vec<&str> = models
        .iter()
        .filter(|m| m.is_listed())
        .map(|m| m.slug.as_str())
        .collect();
    assert_eq!(listed, ["gpt-a"]);
    let recorded = &harness.backend.requests()[0];
    assert_eq!(recorded.path, "/v1/models?client_version=99.0.0");
    assert_eq!(recorded.purpose.as_deref(), Some("models"));
    let header = |name: &str| recorded.headers.get(name).map(String::as_str);
    assert_eq!(header("accept"), Some("application/json"));
    assert_eq!(header("originator"), Some("steno"));
    assert_eq!(header("chatgpt-account-id"), Some("acct_stored"));
    assert_eq!(
        recorded.authorization().map(str::to_owned),
        Some(bearer(&CodexHome::access_token(3_600, "plus")))
    );
    let probe = harness.client.probe_llm().await.unwrap();
    assert_eq!(probe.model_listed, Some(true));
    assert_eq!(probe.resolved_mode, StructuredOutputMode::JsonSchema);
    assert_eq!(
        probe.account_line.as_deref(),
        Some("nicolai@example.com (Plus)")
    );
    assert_eq!(
        harness
            .backend
            .requests()
            .last()
            .unwrap()
            .purpose
            .as_deref(),
        Some("probe")
    );

    let hanging = CodexHarness::build(RetryPolicy::NONE, "gpt-stub", |e| {
        e.request_timeout = secs(5);
    })
    .await;
    hanging.backend.enqueue([StubResponse::hang()]);
    let client =
        CodexResponsesClient::new(hanging.endpoint.clone(), Arc::new(hanging.home.store()))
            .with_retry(RetryPolicy::NONE)
            .with_clock(hanging.clock.clone());
    let task = tokio::spawn(async move { client.complete_llm(&text_request()).await });
    assert!(hanging.clock.wait_for_sleepers(1, SLEEPER_WAIT).await);
    hanging.clock.advance(secs(5));
    let error = llm(task.await.unwrap().unwrap_err());
    assert_eq!(error, LlmError::Timeout);
}

#[test]
fn endpoint_from_settings_needs_confirmation_and_a_model() {
    let mut settings = Settings {
        llm_provider: steno_core::LlmProvider::Codex,
        ..Settings::default()
    };
    assert!(LlmEndpoint::from_settings(&settings).is_none());
    settings.codex_model = Some("gpt-5.6-terra".to_owned());
    assert!(
        LlmEndpoint::from_settings(&settings).is_none(),
        "unconfirmed: never configured"
    );
    settings.codex_confirmed_at = Some(chrono::Utc::now());
    let endpoint = LlmEndpoint::from_settings(&settings).unwrap();
    assert!(endpoint.is_codex_backend());
    assert_eq!(endpoint.model, "gpt-5.6-terra");
    assert_eq!(
        endpoint.context_tokens,
        Settings::DEFAULT_CODEX_CONTEXT_TOKENS
    );
    assert_eq!(endpoint.max_output_tokens, 16_000);
    assert_eq!(
        endpoint.responses_url().as_str(),
        "https://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(
        endpoint.models_url().as_str(),
        "https://chatgpt.com/backend-api/codex/models"
    );
    // The endpoint provider's fields do not leak into the Codex endpoint and
    // switching back finds them untouched.
    settings.llm_base_url = Some("http://127.0.0.1:1234/v1".to_owned());
    settings.llm_model = Some("local".to_owned());
    assert_eq!(
        LlmEndpoint::from_settings(&settings).unwrap().model,
        "gpt-5.6-terra"
    );
    settings.llm_provider = steno_core::LlmProvider::Endpoint;
    let local = LlmEndpoint::from_settings(&settings).unwrap();
    assert_eq!(local.model, "local");
    assert!(!local.is_codex_backend());

    // The confirmation gate cannot be walked around by pasting the
    // backend's address as a server: that stays an endpoint.
    settings.llm_base_url = Some(LlmEndpoint::CODEX_BACKEND_URL.to_owned());
    let pasted = LlmEndpoint::from_settings(&settings).unwrap();
    assert!(!pasted.is_codex_backend());
    assert_eq!(pasted.provider, steno_core::LlmProvider::Endpoint);
    assert!(LlmEndpoint::codex("m", 1).is_codex_backend());

    // The Codex context is clamped and an empty model rejected.
    settings.llm_provider = steno_core::LlmProvider::Codex;
    settings.codex_model = Some(String::new());
    assert!(LlmEndpoint::from_settings(&settings).is_none());
    settings.codex_model = Some("gpt-5.6-terra".to_owned());
    settings.codex_context_tokens = 10;
    assert_eq!(
        LlmEndpoint::from_settings(&settings)
            .unwrap()
            .context_tokens,
        1_024
    );
    settings.codex_context_tokens = 272_000;
    let large = LlmEndpoint::from_settings(&settings).unwrap();
    assert_eq!(large.context_tokens, 272_000);
    assert_eq!(large.max_concurrent_requests, 2);
    assert_eq!(large.request_timeout, secs(240));
    assert_eq!(
        large.structured_output_mode,
        StructuredOutputMode::JsonSchema
    );
    assert_eq!(LlmEndpoint::codex("m", 0).context_tokens, 1_024);
}

#[tokio::test]
async fn server_errors_retry_with_backoff_unless_the_policy_says_otherwise() {
    let harness = CodexHarness::new().await;
    harness.backend.enqueue([
        scripts.server_error(503),
        scripts.server_error(408),
        scripts.stream("ok"),
    ]);
    let driver = harness.drive_retries();
    let response = harness.client.complete_llm(&text_request()).await.unwrap();
    driver.abort();
    assert_eq!(response.text, "ok");
    assert_eq!(harness.backend.request_count(), 3);
    let failed = "The server had an error".to_owned();
    let events = harness.events();
    assert!(events.contains(&LlmClientEvent::Retrying {
        after: secs(2),
        attempt: 1,
        reason: LlmError::Http {
            status: 503,
            body: failed.clone()
        }
    }));
    assert!(events.contains(&LlmClientEvent::Retrying {
        after: secs(4),
        attempt: 2,
        reason: LlmError::Http {
            status: 408,
            body: failed.clone()
        }
    }));
    let auths: std::collections::HashSet<Option<String>> = harness
        .backend
        .requests()
        .iter()
        .map(|r| r.authorization().map(str::to_owned))
        .collect();
    assert_eq!(auths.len(), 1);
    assert_eq!(
        harness.home.server.requests().len(),
        0,
        "a server error is not a sign-in problem"
    );

    let once = CodexHarness::with_retry(RetryPolicy::NONE).await;
    once.backend.enqueue([scripts.server_error(502)]);
    let error = llm(once.client.complete_llm(&text_request()).await.unwrap_err());
    assert_eq!(
        error,
        LlmError::Http {
            status: 502,
            body: failed.clone()
        }
    );
    assert_eq!(once.backend.request_count(), 1);
    assert_eq!(once.clock.pending_sleepers(), 0);

    let exhausted = CodexHarness::new().await;
    exhausted.backend.enqueue([
        scripts.server_error(500),
        scripts.server_error(500),
        scripts.server_error(500),
        scripts.stream("never reached"),
    ]);
    let driver = exhausted.drive_retries();
    let last = llm(exhausted
        .client
        .complete_llm(&text_request())
        .await
        .unwrap_err());
    driver.abort();
    assert_eq!(
        last,
        LlmError::Http {
            status: 500,
            body: failed
        }
    );
    assert_eq!(exhausted.backend.request_count(), 3);
}

#[tokio::test]
async fn a_four_hundred_that_does_not_name_the_format_is_not_downgraded() {
    let harness = CodexHarness::new().await;
    harness
        .backend
        .enqueue([scripts.bad_request("The model `gpt-stub` does not exist")]);
    let schema = request("test", schema_format("r"));
    let unknown_model = llm(harness.client.complete_llm(&schema).await.unwrap_err());
    assert_eq!(
        unknown_model,
        LlmError::Http {
            status: 400,
            body: "The model `gpt-stub` does not exist".to_owned()
        }
    );
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::JsonSchema
    );
    assert_eq!(harness.backend.request_count(), 1);

    harness
        .backend
        .enqueue([scripts.bad_request("Invalid value for text.format")]);
    let plain = llm(harness
        .client
        .complete_llm(&text_request())
        .await
        .unwrap_err());
    assert_eq!(
        plain,
        LlmError::Http {
            status: 400,
            body: "Invalid value for text.format".to_owned()
        }
    );
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::JsonSchema
    );
    assert_eq!(harness.backend.request_count(), 2);
    assert_eq!(harness.clock.pending_sleepers(), 0);

    let floor = CodexHarness::build(RetryPolicy::default(), "gpt-stub", |e| {
        e.structured_output_mode = StructuredOutputMode::PromptOnly;
    })
    .await;
    floor
        .backend
        .enqueue([scripts.bad_request("text.format is not supported")]);
    let last = llm(floor.client.complete_llm(&schema).await.unwrap_err());
    assert_eq!(
        last,
        LlmError::Http {
            status: 400,
            body: "text.format is not supported".to_owned()
        }
    );
    assert_eq!(floor.backend.request_count(), 1);
    assert_eq!(text_format_kinds(&floor), [None]);
    assert_eq!(
        floor.client.resolved_mode(),
        StructuredOutputMode::PromptOnly
    );
}

/// Two chunks in flight under `JsonSchema` both draw the 400: one
/// `ModeDowngraded` per step, and the late second 400 cannot move the mode
/// back up.
#[tokio::test]
async fn concurrent_rejections_downgrade_the_mode_once_per_step() {
    let harness = CodexHarness::new().await;
    harness.backend.respond(Arc::new(|request| {
        let has_text = request.responses.as_ref().is_some_and(|b| b.text.is_some());
        Some(if has_text {
            scripts.bad_request("text.format is not supported by this model")
        } else {
            scripts.stream("{}")
        })
    }));
    harness.backend.hold_responses();
    let first = request("a", schema_format("r"));
    let second = request("b", schema_format("r"));
    let (a, b, ()) = tokio::join!(
        harness.client.complete_llm(&first),
        harness.client.complete_llm(&second),
        async {
            harness.backend.received(2).await;
            harness.backend.release();
        }
    );
    a.unwrap();
    b.unwrap();
    let downgrades: Vec<_> = harness
        .events()
        .into_iter()
        .filter_map(|event| match event {
            LlmClientEvent::ModeDowngraded(mode) => Some(mode),
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
    assert_modes_never_go_up(&harness.events());
    assert_eq!(harness.backend.request_count(), 6);
    assert_eq!(harness.clock.pending_sleepers(), 0);
}

#[tokio::test]
async fn the_downgrade_runs_to_prompt_only_and_sticks() {
    let harness = CodexHarness::new().await;
    harness.backend.respond(Arc::new(|request| {
        let has_text = request.responses.as_ref().is_some_and(|b| b.text.is_some());
        Some(if has_text {
            scripts.bad_request("text.format is not supported by this model")
        } else {
            scripts.stream("{}")
        })
    }));
    harness
        .client
        .complete_llm(&request("test", schema_format("r")))
        .await
        .unwrap();
    assert_eq!(
        text_format_kinds(&harness),
        [
            Some("json_schema".to_owned()),
            Some("json_object".to_owned()),
            None
        ]
    );
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::PromptOnly
    );
    let events = harness.events();
    assert!(events.contains(&LlmClientEvent::ModeDowngraded(
        StructuredOutputMode::JsonObject
    )));
    assert!(events.contains(&LlmClientEvent::ModeDowngraded(
        StructuredOutputMode::PromptOnly
    )));
    assert_eq!(
        harness.clock.pending_sleepers(),
        0,
        "a downgrade is a resend, not a retry"
    );

    harness.client.complete_llm(&json_request()).await.unwrap();
    assert_eq!(harness.backend.request_count(), 4);
    assert_eq!(text_format_kinds(&harness)[3], None);
}

#[tokio::test]
async fn text_format_follows_the_request_and_the_mode() {
    let schema = LlmResponseFormat::JsonSchema {
        name: "r".to_owned(),
        schema: serde_json::json!({"type": "object"}),
        strict: false,
    };
    let text_format = CodexResponsesClient::text_format;
    assert_eq!(
        text_format(&schema, StructuredOutputMode::JsonSchema),
        Some(ResponsesFormat::json_schema(
            "r",
            serde_json::json!({"type": "object"}),
            false
        ))
    );
    assert_eq!(
        text_format(&schema, StructuredOutputMode::JsonObject),
        Some(ResponsesFormat::json_object())
    );
    assert_eq!(text_format(&schema, StructuredOutputMode::PromptOnly), None);
    assert_eq!(
        text_format(
            &LlmResponseFormat::JsonObject,
            StructuredOutputMode::JsonSchema
        ),
        Some(ResponsesFormat::json_object())
    );
    assert_eq!(
        text_format(
            &LlmResponseFormat::JsonObject,
            StructuredOutputMode::JsonObject
        ),
        Some(ResponsesFormat::json_object())
    );
    assert_eq!(
        text_format(
            &LlmResponseFormat::JsonObject,
            StructuredOutputMode::PromptOnly
        ),
        None
    );
    for mode in StructuredOutputMode::ALL {
        assert_eq!(
            text_format(&LlmResponseFormat::Text, mode),
            None,
            "{mode:?}"
        );
    }

    let harness = CodexHarness::new().await;
    harness.backend.enqueue([scripts.stream("{}")]);
    harness.client.complete_llm(&json_request()).await.unwrap();
    let recorded = &harness.backend.requests()[0];
    let body = recorded.responses.as_ref().unwrap();
    assert_eq!(
        body.text.as_ref().unwrap().format,
        ResponsesFormat::json_object()
    );
    let raw = recorded.body_text();
    assert!(!raw.contains("schema"));
    assert!(!raw.contains("strict"));
    assert_eq!(body.reasoning.as_ref().unwrap().effort, "medium");
}

#[tokio::test]
async fn probe_throws_when_the_completion_fails_and_tolerates_a_failed_model_list() {
    let harness = CodexHarness::with_retry(RetryPolicy::NONE).await;
    let models = || scripts.codex_models(vec![CodexModel::new("gpt-stub", "Stub")]);
    harness.backend.respond(Arc::new(move |request| {
        Some(if request.method == "GET" {
            models()
        } else {
            StubResponse::json(
                &ChatErrorEnvelope::message("model `gpt-stub` does not exist"),
                404,
            )
        })
    }));
    let not_found = llm(harness.client.probe_llm().await.unwrap_err());
    assert_eq!(
        not_found,
        LlmError::Http {
            status: 404,
            body: "model `gpt-stub` does not exist".to_owned()
        }
    );
    assert_eq!(harness.backend.request_count(), 2);

    harness.backend.respond(Arc::new(move |request| {
        Some(if request.method == "GET" {
            models()
        } else {
            scripts.responses_failed("boom")
        })
    }));
    let failed = llm(harness.client.probe_llm().await.unwrap_err());
    assert_eq!(failed, LlmError::Transport("boom".to_owned()));
    assert_eq!(harness.backend.request_count(), 4);

    harness.backend.respond(Arc::new(|request| {
        Some(if request.method == "GET" {
            scripts.unauthorized()
        } else {
            scripts.stream("{\"ok\":true}")
        })
    }));
    let probe = harness.client.probe_llm().await.unwrap();
    assert_eq!(probe.model_listed, None);
    assert_eq!(
        probe.account_line.as_deref(),
        Some("nicolai@example.com (Plus)")
    );
    assert_eq!(harness.backend.request_count(), 6);

    harness.backend.respond(Arc::new(|request| {
        Some(if request.method == "GET" {
            scripts.raw_completion("<html>")
        } else {
            scripts.stream("{}")
        })
    }));
    let undecodable = llm(harness.client.list_models().await.unwrap_err());
    assert_eq!(
        undecodable,
        LlmError::Transport("undecodable model list: <html>".to_owned())
    );

    std::fs::remove_file(harness.home.file()).unwrap();
    let no_sign_in = credential(harness.client.probe_llm().await.unwrap_err());
    assert_eq!(no_sign_in, CodexCredentialError::NotSignedIn);
    assert_eq!(
        harness.backend.request_count(),
        7,
        "no request without a sign-in"
    );
    assert_eq!(harness.clock.pending_sleepers(), 0);
}

#[tokio::test]
async fn stream_errors_and_dropped_connections_are_redacted_transport_errors() {
    let harness = CodexHarness::with_retry(RetryPolicy::NONE).await;
    let token = CodexHome::access_token(3_600, "plus");
    harness.backend.enqueue([scripts.responses_error_event(
        &format!("upstream rejected {token} for acct_stored"),
        "server_error",
    )]);
    let error = llm(harness
        .client
        .complete_llm(&text_request())
        .await
        .unwrap_err());
    assert_eq!(
        error,
        LlmError::Transport("upstream rejected [redacted] for [redacted]".to_owned())
    );
    assert!(error.is_retryable());

    harness.backend.enqueue([StubResponse::drop_connection()]);
    let dropped = llm(harness
        .client
        .complete_llm(&text_request())
        .await
        .unwrap_err());
    let LlmError::Transport(message) = &dropped else {
        panic!("expected a transport error, got {dropped}");
    };
    assert!(!message.contains(&token));
    assert!(!message.contains("acct_stored"));
    for event in harness.events() {
        let text = format!("{event:?}");
        assert!(!text.contains(&token), "{text}");
        assert!(!text.contains("acct_stored"), "{text}");
    }
}

#[tokio::test]
async fn data_only_streams_and_plain_json_replies_are_parsed() {
    let harness = CodexHarness::with_retry(RetryPolicy::NONE).await;
    harness.backend.enqueue([scripts.raw_event_stream(
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"typed\"}]}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}\n\n",
    )]);
    let typed = harness.client.complete_llm(&text_request()).await.unwrap();
    assert_eq!(typed.text, "typed");
    assert_eq!(
        typed.usage,
        Some(LlmUsage {
            prompt_tokens: 1,
            completion_tokens: 2,
            requests: 1
        })
    );

    harness.backend.enqueue([StubResponse::json(
        &ResponsesResponse {
            id: Some("resp_1".to_owned()),
            status: Some("incomplete".to_owned()),
            model: Some("gpt-json".to_owned()),
            output: Some(vec![ResponsesOutputItem::message(None, "whole")]),
            usage: Some(ResponsesUsage {
                input_tokens: Some(5),
                output_tokens: Some(6),
            }),
            incomplete_details: Some(ResponsesIncompleteDetails {
                reason: Some("max_output_tokens".to_owned()),
            }),
            error: None,
        },
        200,
    )]);
    let whole = harness.client.complete_llm(&text_request()).await.unwrap();
    assert_eq!(whole.text, "whole");
    assert_eq!(whole.finish_reason, LlmFinishReason::Length);
    assert_eq!(whole.model.as_deref(), Some("gpt-json"));

    harness
        .backend
        .enqueue([scripts.raw_completion("<html>not an API</html>")]);
    let undecodable = llm(harness
        .client
        .complete_llm(&text_request())
        .await
        .unwrap_err());
    assert_eq!(
        undecodable,
        LlmError::Transport("undecodable response body: <html>not an API</html>".to_owned())
    );
    assert_eq!(harness.clock.pending_sleepers(), 0);
}

/// A plan limit delivered inside the stream is as final as one on the
/// status line: no backoff. An overload is retried like a dropped
/// connection.
#[tokio::test]
async fn a_plan_limit_inside_the_stream_is_final() {
    let harness = CodexHarness::new().await;
    harness.backend.enqueue([
        scripts.responses_error_event("Your plan does not include this.", "usage_not_included")
    ]);
    let error = llm(harness
        .client
        .complete_llm(&text_request())
        .await
        .unwrap_err());
    assert_eq!(
        error,
        LlmError::Http {
            status: 400,
            body: "ChatGPT plan limit reached: Your plan does not include this.".to_owned()
        }
    );
    assert!(!error.is_retryable());
    assert_eq!(harness.backend.request_count(), 1);

    let overloaded = CodexHarness::new().await;
    overloaded.backend.enqueue([
        scripts.responses_error_event("busy", "server_is_overloaded"),
        scripts.stream("ok"),
    ]);
    let driver = overloaded.drive_retries();
    let response = overloaded
        .client
        .complete_llm(&text_request())
        .await
        .unwrap();
    driver.abort();
    assert_eq!(
        response.text, "ok",
        "an overload is retried like a dropped connection"
    );
    assert_eq!(overloaded.backend.request_count(), 2);
}

/// A 5xx from the token endpoint is a transport failure: one backoff, then
/// the refresh is tried again. A spent token stays final.
#[tokio::test]
async fn a_token_endpoint_hiccup_backs_off_like_a_transport_error() {
    let harness = CodexHarness::new().await;
    harness
        .home
        .write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    let fresh = CodexHome::access_token(3_600, "plus");
    harness.home.server.enqueue([
        scripts.server_error(503),
        scripts.token_refresh(&fresh, Some("rt_2"), None),
    ]);
    harness.backend.enqueue([scripts.stream("ok")]);
    let driver = harness.drive_retries();
    let response = harness.client.complete_llm(&text_request()).await.unwrap();
    driver.abort();
    assert_eq!(response.text, "ok");
    assert_eq!(harness.home.server.request_count(), 2);
    assert_eq!(
        harness.backend.requests()[0]
            .authorization()
            .map(str::to_owned),
        Some(bearer(&fresh))
    );
    assert!(harness.events().iter().any(|event| matches!(
        event,
        LlmClientEvent::Retrying {
            attempt: 1,
            reason: LlmError::Transport(_),
            ..
        }
    )));

    let expired = CodexHarness::new().await;
    expired
        .home
        .write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    expired
        .home
        .server
        .enqueue([scripts.token_refresh_rejected("refresh_token_expired", 400)]);
    let error = credential(
        expired
            .client
            .complete_llm(&text_request())
            .await
            .unwrap_err(),
    );
    assert!(
        matches!(error, CodexCredentialError::SignInExpired(_)),
        "{error}"
    );
    assert_eq!(expired.backend.requests().len(), 0);
}

/// A 401 when the CLI has rotated the file meanwhile: the file's token goes
/// out next, the token endpoint is not called.
#[tokio::test]
async fn unauthorized_with_a_rotated_file_rereads_instead_of_refreshing() {
    let harness = CodexHarness::new().await;
    let rotated = CodexHome::access_token(3_600, "pro");
    let file = harness.home.file();
    let rotated_for_responder = rotated.clone();
    harness.backend.respond(Arc::new(move |request| {
        if request.index == 0 {
            // The CLI rotates the file while the first request is in flight.
            let auth = AuthFile::default()
                .access(&rotated_for_responder)
                .refresh("rt_cli");
            write_auth(&file, auth);
            return Some(scripts.unauthorized());
        }
        Some(scripts.stream("ok"))
    }));
    let response = harness.client.complete_llm(&text_request()).await.unwrap();
    assert_eq!(response.text, "ok");
    let auths: Vec<Option<String>> = harness
        .backend
        .requests()
        .iter()
        .map(|r| r.authorization().map(str::to_owned))
        .collect();
    assert_eq!(
        auths,
        [
            Some(bearer(&CodexHome::access_token(3_600, "plus"))),
            Some(bearer(&rotated))
        ]
    );
    assert_eq!(harness.home.server.requests().len(), 0);
}
