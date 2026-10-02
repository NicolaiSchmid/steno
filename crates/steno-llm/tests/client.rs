//! The OpenAI-compatible client against the stub server: wire format, auth,
//! retries on the manual clock, mode and parameter fallback, redaction.
//! Swift: `ClientTests`, `ClientContractTests`, `RetryTests`,
//! `ModeFallbackTests`, `ParameterFallbackTests`.

mod common;

use std::collections::HashMap;
use std::time::Duration;

use common::*;
use steno_core::{LlmFinishReason, LlmResponseFormat, LlmUsage, Settings};
use steno_llm::testing::{StubResponse, scripts};
use steno_llm::transport::{self, HttpReply};
use steno_llm::{
    LlmClient, LlmClientEvent, LlmEndpoint, LlmError, OpenAiCompatibleClient, RetryPolicy,
    StructuredOutputMode,
};

#[tokio::test]
async fn sends_bearer_auth_body_and_purpose_and_parses_the_reply() {
    let harness = ClientHarness::new().await;
    harness.server.enqueue([scripts.completion(
        "{\"hi\":true}",
        Some("stop"),
        Some(LlmUsage {
            prompt_tokens: 12,
            completion_tokens: 4,
            requests: 1,
        }),
        "served-model",
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
    assert_eq!(response.model.as_deref(), Some("served-model"));

    let recorded = &harness.server.requests()[0];
    assert_eq!(recorded.path, "/v1/chat/completions");
    assert_eq!(
        recorded.authorization(),
        Some(format!("Bearer {API_KEY}").as_str())
    );
    assert_eq!(recorded.purpose.as_deref(), Some("cleanup"));
    assert_eq!(
        recorded.headers.get("content-type").map(String::as_str),
        Some("application/json")
    );
    let chat = recorded.chat.as_ref().unwrap();
    assert_eq!(chat.model, "stub-model");
    let roles: Vec<&str> = chat.messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, ["system", "user"]);
    assert_eq!(chat.temperature, Some(0.0));
    assert_eq!(chat.max_tokens, Some(64));
    let format = chat.response_format.as_ref().unwrap();
    assert_eq!(format.kind, "json_schema");
    assert!(format.json_schema.as_ref().unwrap().strict);
    assert_eq!(format.json_schema.as_ref().unwrap().name, "reply");
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::JsonSchema
    );
}

#[tokio::test]
async fn omits_authorization_without_a_key_and_defaults_max_tokens() {
    let harness =
        ClientHarness::build(RetryPolicy::default(), None, |e| e.max_output_tokens = 777).await;
    harness.server.enqueue([scripts.text("ok")]);
    let mut request = text_request();
    request.max_tokens = None;
    harness.client.complete_llm(&request).await.unwrap();
    let recorded = &harness.server.requests()[0];
    assert_eq!(recorded.authorization(), None);
    assert_eq!(recorded.chat.as_ref().unwrap().max_tokens, Some(777));
    assert!(recorded.chat.as_ref().unwrap().response_format.is_none());
}

#[tokio::test]
async fn an_empty_key_is_no_key() {
    let harness = ClientHarness::with_key(Some("")).await;
    harness.server.enqueue([scripts.text("ok")]);
    harness.client.complete_llm(&text_request()).await.unwrap();
    let recorded = &harness.server.requests()[0];
    assert_eq!(recorded.authorization(), None);
    assert_eq!(
        recorded.headers.get("user-agent").map(String::as_str),
        Some(steno_llm::USER_AGENT)
    );
    assert_eq!(
        recorded.headers.get("accept").map(String::as_str),
        Some("application/json")
    );
}

#[tokio::test]
async fn unauthorized_is_not_retried() {
    let harness = ClientHarness::new().await;
    harness.server.enqueue([scripts.unauthorized()]);
    let error = harness
        .client
        .complete_llm(&json_request())
        .await
        .unwrap_err();
    assert_eq!(
        error,
        LlmError::Http {
            status: 401,
            body: "Incorrect API key provided".to_owned()
        }
    );
    assert_eq!(harness.server.request_count(), 1);
    assert_eq!(harness.clock.pending_sleepers(), 0);
}

#[tokio::test]
async fn length_and_refusal_surface_as_finish_reason_and_error() {
    let harness = ClientHarness::new().await;
    harness
        .server
        .enqueue([scripts.truncated("{\"segments\": [{\"index\": 0, \"te")]);
    let response = harness.client.complete_llm(&json_request()).await.unwrap();
    assert_eq!(response.finish_reason, LlmFinishReason::Length);

    harness
        .server
        .enqueue([scripts.refusal("I cannot help with that.")]);
    let error = harness
        .client
        .complete_llm(&json_request())
        .await
        .unwrap_err();
    assert_eq!(
        error,
        LlmError::Refused("I cannot help with that.".to_owned())
    );
}

#[tokio::test]
async fn cancelling_the_call_sends_nothing_more() {
    let harness = ClientHarness::new().await;
    harness.server.enqueue([StubResponse::hang()]);
    let request = json_request();
    let call = tokio::spawn({
        // A client the task owns; the one in the harness stays for assertions.
        let client = OpenAiCompatibleClient::new(harness.endpoint.clone(), Some(API_KEY))
            .with_clock(harness.clock.clone());
        async move { client.complete_llm(&request).await }
    });
    harness.server.received(1).await;
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    tokio::task::yield_now().await;
    assert_eq!(harness.server.request_count(), 1);
}

#[tokio::test]
async fn redacts_the_key_from_every_error_and_event() {
    let harness = ClientHarness::with_retry(RetryPolicy::NONE).await;
    harness
        .server
        .enqueue([scripts.bad_request(&format!("rejected key {API_KEY} for this model"))]);
    let http = harness
        .client
        .complete_llm(&text_request())
        .await
        .unwrap_err();
    assert_eq!(
        http,
        LlmError::Http {
            status: 400,
            body: "rejected key [redacted] for this model".to_owned()
        }
    );
    harness
        .server
        .enqueue([scripts.refusal(&format!("no, {API_KEY}"))]);
    let refused = harness
        .client
        .complete_llm(&json_request())
        .await
        .unwrap_err();
    assert_eq!(refused, LlmError::Refused("no, [redacted]".to_owned()));

    let every_case = [
        http,
        LlmError::Transport("boom".to_owned()),
        LlmError::Timeout,
        LlmError::RateLimited {
            retry_after: Some(secs(3)),
        },
        LlmError::RateLimited { retry_after: None },
        LlmError::InvalidJson("x".to_owned()),
        LlmError::Truncated,
        refused,
        LlmError::TranscriptTooLong {
            estimated_tokens: 1,
            budget: 2,
        },
    ];
    for error in every_case {
        assert!(!error.to_string().contains(API_KEY), "{error}");
        assert_ne!(error.to_string().len(), 0);
    }
    for event in harness.events() {
        assert!(!format!("{event:?}").contains(API_KEY), "{event:?}");
    }
    assert_eq!(
        transport::redact(&format!("a {API_KEY} b"), &[API_KEY.to_owned()]),
        "a [redacted] b"
    );
    assert_eq!(transport::redact("a b", &[]), "a b");
}

#[tokio::test]
async fn probe_reports_models_mode_and_round_trip() {
    let harness = ClientHarness::new().await;
    harness.server.respond(scripts.server(
        &["other", "stub-model"],
        &[],
        scripts.text("{\"ok\":true}"),
    ));
    let probe = harness.client.probe_llm().await.unwrap();
    assert_eq!(probe.model_listed, Some(true));
    assert_eq!(probe.resolved_mode, StructuredOutputMode::JsonSchema);
    assert_eq!(probe.round_trip, Duration::ZERO);
    assert_eq!(probe.account_line, None);
    let paths: Vec<String> = harness
        .server
        .requests()
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    assert_eq!(paths, ["GET /v1/models", "POST /v1/chat/completions"]);
    assert_eq!(
        harness.server.requests().last().unwrap().purpose.as_deref(),
        Some("probe")
    );
}

#[tokio::test]
async fn probe_without_a_model_list_still_succeeds() {
    let harness = ClientHarness::new().await;
    harness.server.respond(std::sync::Arc::new(|request| {
        Some(if request.method == "GET" {
            scripts.bad_request("no such route")
        } else {
            scripts.text("{\"ok\":true}")
        })
    }));
    let probe = harness.client.probe_llm().await.unwrap();
    assert_eq!(probe.model_listed, None);
}

#[tokio::test]
async fn probe_throws_on_401_and_when_the_completion_fails_however_the_model_list_answered() {
    let harness = ClientHarness::with_retry(RetryPolicy::NONE).await;
    harness.server.respond(std::sync::Arc::new(|request| {
        Some(if request.method == "GET" {
            scripts.models(&["stub-model"])
        } else {
            scripts.unauthorized()
        })
    }));
    let error = harness.client.probe_llm().await.unwrap_err();
    assert_eq!(
        error,
        LlmError::Http {
            status: 401,
            body: "Incorrect API key provided".to_owned()
        }
    );
    assert_eq!(harness.server.request_count(), 2);

    harness.server.respond(std::sync::Arc::new(|request| {
        Some(if request.method == "GET" {
            scripts.models(&["stub-model"])
        } else {
            StubResponse::json(
                &steno_llm::wire::ChatErrorEnvelope::message("model `stub-model` does not exist"),
                404,
            )
        })
    }));
    let not_found = harness.client.probe_llm().await.unwrap_err();
    assert_eq!(
        not_found,
        LlmError::Http {
            status: 404,
            body: "model `stub-model` does not exist".to_owned()
        }
    );
    assert_eq!(harness.server.request_count(), 4);

    harness.server.respond(std::sync::Arc::new(|request| {
        Some(if request.method == "GET" {
            scripts.models(&["stub-model"])
        } else {
            scripts.raw_completion("<html>not an API</html>")
        })
    }));
    let undecodable = harness.client.probe_llm().await.unwrap_err();
    assert_eq!(
        undecodable,
        LlmError::Transport("undecodable completion body: <html>not an API</html>".to_owned())
    );
    assert_eq!(harness.server.request_count(), 6);
    assert_eq!(harness.clock.pending_sleepers(), 0);
}

#[tokio::test]
async fn probe_throws_a_transport_error_when_nothing_listens() {
    // Port 1 (tcpmux) is closed on every runner: the connection is refused.
    let endpoint = LlmEndpoint::new(url::Url::parse("http://127.0.0.1:1/v1").unwrap(), "m");
    let client = OpenAiCompatibleClient::new(endpoint, None).with_retry(RetryPolicy::NONE);
    let error = client.probe_llm().await.unwrap_err();
    assert!(matches!(error, LlmError::Transport(_)), "{error}");
}

#[tokio::test]
async fn probe_records_the_downgraded_mode_and_a_model_that_is_not_listed() {
    let harness = ClientHarness::new().await;
    harness.server.respond(scripts.server(
        &["other"],
        &["json_schema"],
        scripts.text("{\"ok\":true}"),
    ));
    let probe = harness.client.probe_llm().await.unwrap();
    assert_eq!(probe.model_listed, Some(false));
    assert_eq!(probe.resolved_mode, StructuredOutputMode::JsonObject);
    assert_eq!(
        harness.server.request_count(),
        3,
        "GET, rejected json_schema, json_object"
    );
    assert_eq!(harness.clock.pending_sleepers(), 0);
}

#[test]
fn endpoint_from_settings_needs_both_url_and_model_and_clamps_the_context() {
    let mut settings = Settings::default();
    assert!(LlmEndpoint::from_settings(&settings).is_none());
    settings.llm_base_url = Some("http://127.0.0.1:1234/v1/".to_owned());
    assert!(LlmEndpoint::from_settings(&settings).is_none());
    settings.llm_model = Some(String::new());
    assert!(LlmEndpoint::from_settings(&settings).is_none());
    settings.llm_model = Some("local-model".to_owned());
    settings.llm_context_tokens = 16_000;
    let endpoint = LlmEndpoint::from_settings(&settings).unwrap();
    assert_eq!(endpoint.model, "local-model");
    assert_eq!(endpoint.context_tokens, 16_000);
    assert_eq!(
        endpoint.chat_completions_url().as_str(),
        "http://127.0.0.1:1234/v1/chat/completions"
    );
    assert_eq!(
        endpoint.models_url().as_str(),
        "http://127.0.0.1:1234/v1/models"
    );
    assert_eq!(
        endpoint.structured_output_mode,
        StructuredOutputMode::JsonSchema
    );
    assert_eq!(endpoint.max_concurrent_requests, 2);
    assert_eq!(endpoint.request_timeout, secs(240));
    settings.llm_context_tokens = 10;
    assert_eq!(
        LlmEndpoint::from_settings(&settings)
            .unwrap()
            .context_tokens,
        1_024
    );
}

#[test]
fn retry_after_and_error_message_parsing() {
    assert_eq!(transport::retry_after(Some("7")), Some(secs(7)));
    assert_eq!(
        transport::retry_after(Some(" 1.5 ")),
        Some(Duration::from_millis(1500))
    );
    assert_eq!(
        transport::retry_after(Some("Wed, 21 Oct 2026 07:28:00 GMT")),
        None
    );
    assert_eq!(transport::retry_after(None), None);
    assert_eq!(transport::retry_after(Some("inf")), None);
    assert_eq!(transport::retry_after(Some("infinity")), None);
    assert_eq!(transport::retry_after(Some("nan")), None);
    assert_eq!(transport::retry_after(Some("-1")), None);
    assert_eq!(transport::retry_after(Some("1e300")), Some(secs(3_600)));
    assert_eq!(transport::retry_after(Some("1e19")), Some(secs(3_600)));
    assert_eq!(transport::retry_after(Some("3601")), Some(secs(3_600)));
    assert!(OpenAiCompatibleClient::complains_about_response_format(
        "Invalid parameter: 'response_format'"
    ));
    assert!(OpenAiCompatibleClient::complains_about_response_format(
        "json_schema is not supported"
    ));
    assert!(!OpenAiCompatibleClient::complains_about_response_format(
        "model not found"
    ));
    let html = HttpReply {
        status: 502,
        headers: HashMap::new(),
        body: b"<html>bad gateway</html>".to_vec(),
    };
    assert_eq!(
        OpenAiCompatibleClient::error_message(&html),
        "<html>bad gateway</html>"
    );
}

#[test]
fn retryability_matrix() {
    for status in [500, 502, 503, 504, 599, 408] {
        assert!(
            LlmError::Http {
                status,
                body: String::new()
            }
            .is_retryable(),
            "{status}"
        );
    }
    for status in [400, 401, 403, 404, 413, 422, 429, 499, 200] {
        assert!(
            !LlmError::Http {
                status,
                body: String::new()
            }
            .is_retryable(),
            "{status}"
        );
    }
    assert!(LlmError::RateLimited { retry_after: None }.is_retryable());
    assert!(LlmError::Transport("x".to_owned()).is_retryable());
    assert!(LlmError::Timeout.is_retryable());
    for error in [
        LlmError::InvalidJson("x".to_owned()),
        LlmError::Truncated,
        LlmError::Refused("x".to_owned()),
    ] {
        assert!(!error.is_retryable(), "{error}");
        assert!(error.is_answer_problem(), "{error}");
    }
    assert!(
        !LlmError::TranscriptTooLong {
            estimated_tokens: 1,
            budget: 1
        }
        .is_retryable()
    );
    assert!(
        !LlmError::Http {
            status: 500,
            body: String::new()
        }
        .is_answer_problem()
    );
    assert!(!LlmError::Timeout.is_answer_problem());
}

#[test]
fn wall_clock_backstop_is_twice_the_timeout_and_at_least_thirty_seconds() {
    assert_eq!(transport::wall_clock_backstop(secs(240)), secs(480));
    assert_eq!(transport::wall_clock_backstop(secs(30)), secs(60));
    assert_eq!(transport::wall_clock_backstop(secs(5)), secs(30));
    assert_eq!(
        transport::wall_clock_backstop(Duration::from_millis(1_500)),
        secs(30)
    );
    assert_eq!(transport::wall_clock_backstop(Duration::ZERO), secs(30));
}

#[tokio::test]
async fn a_body_without_usage_still_counts_one_request() {
    let harness = ClientHarness::new().await;
    harness
        .server
        .enqueue([scripts.completion("{}", Some("stop"), None, "stub-model")]);
    let response = harness.client.complete_llm(&json_request()).await.unwrap();
    assert_eq!(
        response.usage,
        Some(LlmUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            requests: 1
        })
    );
    assert_eq!(steno_llm::budget::counted_usage(&response).requests, 1);
}

#[tokio::test]
async fn a_null_or_partial_usage_block_never_fails_a_good_answer() {
    let harness = ClientHarness::new().await;
    let body = |usage: &str| {
        format!(
            "{{\"id\":\"x\",\"object\":\"chat.completion\",\"model\":\"m\",\"choices\":[{{\"index\":0,\"message\":{{\"role\":\"assistant\",\"content\":\"{{\\\"ok\\\":true}}\"}},\"finish_reason\":\"stop\"}}],\"usage\":{usage}}}"
        )
    };
    harness.server.enqueue([
        scripts.raw_completion(&body(
            "{\"prompt_tokens\":null,\"completion_tokens\":null,\"total_tokens\":2}",
        )),
        scripts.raw_completion(&body("{\"total_tokens\":2}")),
        scripts.raw_completion(&body("{\"prompt_tokens\":7}")),
        scripts.raw_completion(&body("null")),
    ]);
    let mut usages = Vec::new();
    for _ in 0..4 {
        let response = harness.client.complete_llm(&json_request()).await.unwrap();
        assert_eq!(response.text, "{\"ok\":true}");
        usages.push(response.usage.unwrap());
    }
    let usage = |p, c| LlmUsage {
        prompt_tokens: p,
        completion_tokens: c,
        requests: 1,
    };
    assert_eq!(usages, [usage(0, 0), usage(0, 0), usage(7, 0), usage(0, 0)]);
    assert_eq!(harness.server.request_count(), 4, "no retries");
    assert_eq!(harness.clock.pending_sleepers(), 0);
    assert!(!harness.events().iter().any(is_retrying));
}

// Retries

#[test]
fn policy_backs_off_exponentially_and_honours_retry_after_within_the_cap() {
    let policy = RetryPolicy::default();
    assert_eq!(policy.max_attempts, 3);
    assert_eq!(policy.delay(1, None), secs(2));
    assert_eq!(policy.delay(2, None), secs(4));
    assert_eq!(policy.delay(3, None), secs(8));
    assert_eq!(policy.delay(10, None), secs(30));
    assert_eq!(policy.delay(1, Some(secs(7))), secs(7));
    assert_eq!(policy.delay(1, Some(secs(600))), secs(30));
    assert_eq!(RetryPolicy::with_max_attempts(0).max_attempts, 1);
    assert_eq!(RetryPolicy::NONE.max_attempts, 1);
}

#[tokio::test]
async fn rate_limit_then_success_honours_retry_after_on_the_clock() {
    let harness = ClientHarness::new().await;
    harness
        .server
        .enqueue([scripts.rate_limited(Some("7")), scripts.text("{}")]);
    let mut events = harness.recorder.take_receiver();
    let client = OpenAiCompatibleClient::new(harness.endpoint.clone(), Some(API_KEY))
        .with_clock(harness.clock.clone())
        .with_observer(harness.recorder.observer());
    let task = tokio::spawn(async move { client.complete_llm(&json_request()).await });

    let retrying = EventRecorder::next(&mut events, is_retrying).await;
    assert_eq!(
        retrying,
        Some(LlmClientEvent::Retrying {
            after: secs(7),
            attempt: 1,
            reason: LlmError::RateLimited {
                retry_after: Some(secs(7))
            }
        })
    );
    assert!(harness.clock.wait_for_sleepers(1, SLEEPER_WAIT).await);
    assert_eq!(harness.server.request_count(), 1);

    harness.clock.advance(secs(6));
    assert_eq!(
        harness.clock.pending_sleepers(),
        1,
        "still asleep one second before Retry-After"
    );
    assert_eq!(harness.server.request_count(), 1);

    harness.clock.advance(secs(1));
    let response = task.await.unwrap().unwrap();
    assert_eq!(response.text, "{}");
    assert_eq!(harness.server.request_count(), 2);
}

#[tokio::test]
async fn three_server_errors_throw_http_500_after_two_backoffs() {
    let harness = ClientHarness::new().await;
    harness.server.enqueue([
        scripts.server_error(500),
        scripts.server_error(500),
        scripts.server_error(500),
    ]);
    let driver = harness.drive_retries();
    let error = harness
        .client
        .complete_llm(&json_request())
        .await
        .unwrap_err();
    driver.abort();
    assert_eq!(
        error,
        LlmError::Http {
            status: 500,
            body: "The server had an error".to_owned()
        }
    );
    assert_eq!(harness.server.request_count(), 3);
    assert_eq!(retry_delays(&harness.events()), [secs(2), secs(4)]);
    assert_eq!(harness.clock.offset(), secs(6));
}

#[tokio::test]
async fn timeout_on_the_clock_cancels_the_attempt_and_retries() {
    let harness = ClientHarness::configured(|e| e.request_timeout = secs(30)).await;
    harness
        .server
        .enqueue([StubResponse::hang(), scripts.text("late")]);
    let driver = harness.drive_retries();
    let client = OpenAiCompatibleClient::new(harness.endpoint.clone(), Some(API_KEY))
        .with_clock(harness.clock.clone())
        .with_observer(harness.recorder.observer());
    let task = tokio::spawn(async move { client.complete_llm(&json_request()).await });
    harness.server.received(1).await;
    assert!(
        harness.clock.wait_for_sleepers(1, SLEEPER_WAIT).await,
        "the timeout sleeper is registered"
    );
    harness.clock.advance(secs(30));
    let response = task.await.unwrap().unwrap();
    driver.abort();
    assert_eq!(response.text, "late");
    assert_eq!(harness.server.request_count(), 2);
    let events = harness.events();
    assert!(events.contains(&LlmClientEvent::TimedOut { attempt: 1 }));
    assert!(events.contains(&LlmClientEvent::Retrying {
        after: secs(2),
        attempt: 1,
        reason: LlmError::Timeout
    }));
}

#[tokio::test]
async fn dropped_connection_is_a_transport_error_and_retried() {
    let harness = ClientHarness::new().await;
    harness
        .server
        .enqueue([StubResponse::drop_connection(), scripts.text("again")]);
    let driver = harness.drive_retries();
    let response = harness.client.complete_llm(&json_request()).await.unwrap();
    driver.abort();
    assert_eq!(response.text, "again");
    assert_eq!(harness.server.request_count(), 2);
    let reasons: Vec<LlmError> = harness
        .events()
        .into_iter()
        .filter_map(|event| match event {
            LlmClientEvent::Retrying { reason, .. } => Some(reason),
            _ => None,
        })
        .collect();
    assert_eq!(reasons.len(), 1);
    assert!(matches!(reasons[0], LlmError::Transport(_)), "{reasons:?}");
}

#[tokio::test]
async fn request_timeout_is_retried_on_408() {
    let harness = ClientHarness::new().await;
    harness
        .server
        .enqueue([scripts.server_error(408), scripts.text("ok")]);
    let driver = harness.drive_retries();
    let response = harness.client.complete_llm(&json_request()).await.unwrap();
    driver.abort();
    assert_eq!(response.text, "ok");
    assert_eq!(harness.server.request_count(), 2);
}

#[tokio::test]
async fn retry_policy_none_never_sleeps() {
    let harness = ClientHarness::with_retry(RetryPolicy::NONE).await;
    harness.server.enqueue([scripts.rate_limited(None)]);
    let error = harness
        .client
        .complete_llm(&json_request())
        .await
        .unwrap_err();
    assert_eq!(error, LlmError::RateLimited { retry_after: None });
    assert_eq!(harness.server.request_count(), 1);
    assert_eq!(harness.clock.pending_sleepers(), 0);
}

#[tokio::test]
async fn cancellation_during_the_backoff_sends_nothing_more() {
    let harness = ClientHarness::new().await;
    harness
        .server
        .enqueue([scripts.server_error(500), scripts.text("never")]);
    let mut events = harness.recorder.take_receiver();
    let client = OpenAiCompatibleClient::new(harness.endpoint.clone(), Some(API_KEY))
        .with_clock(harness.clock.clone())
        .with_observer(harness.recorder.observer());
    let task = tokio::spawn(async move { client.complete_llm(&json_request()).await });
    assert!(
        EventRecorder::next(&mut events, is_retrying)
            .await
            .is_some()
    );
    assert!(harness.clock.wait_for_sleepers(1, SLEEPER_WAIT).await);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(harness.server.request_count(), 1);
    assert_eq!(
        harness.clock.pending_sleepers(),
        0,
        "the sleeper was removed on cancel"
    );
}

#[tokio::test]
async fn a_rate_limit_without_retry_after_uses_the_backoff() {
    let harness = ClientHarness::new().await;
    harness.server.enqueue([
        scripts.rate_limited(None),
        scripts.rate_limited(None),
        scripts.text("ok"),
    ]);
    let driver = harness.drive_retries();
    let response = harness.client.complete_llm(&json_request()).await.unwrap();
    driver.abort();
    assert_eq!(response.text, "ok");
    let delays: Vec<Duration> = harness
        .events()
        .into_iter()
        .filter_map(|event| match event {
            LlmClientEvent::Retrying {
                after,
                reason: LlmError::RateLimited { retry_after: None },
                ..
            } => Some(after),
            _ => None,
        })
        .collect();
    assert_eq!(delays, [secs(2), secs(4)]);
    assert_eq!(harness.server.request_count(), 3);
}

#[tokio::test]
async fn a_retry_after_past_the_cap_is_clamped_by_the_client() {
    let harness = ClientHarness::with_retry(RetryPolicy::new(2, secs(2), secs(5))).await;
    harness
        .server
        .enqueue([scripts.rate_limited(Some("600")), scripts.text("ok")]);
    let driver = harness.drive_retries();
    harness.client.complete_llm(&json_request()).await.unwrap();
    driver.abort();
    assert!(harness.events().contains(&LlmClientEvent::Retrying {
        after: secs(5),
        attempt: 1,
        reason: LlmError::RateLimited {
            retry_after: Some(secs(600))
        }
    }));
    assert_eq!(harness.clock.offset(), secs(5));
}

#[tokio::test]
async fn a_retry_after_the_client_cannot_represent_falls_back_to_the_backoff() {
    let harness = ClientHarness::new().await;
    harness.server.enqueue([
        scripts.rate_limited(Some("inf")),
        scripts.rate_limited(Some("1e300")),
        scripts.text("ok"),
    ]);
    let driver = harness.drive_retries();
    let response = harness.client.complete_llm(&json_request()).await.unwrap();
    driver.abort();
    assert_eq!(response.text, "ok");
    assert_eq!(harness.server.request_count(), 3);
    let events = harness.events();
    assert!(
        events.contains(&LlmClientEvent::Retrying {
            after: secs(2),
            attempt: 1,
            reason: LlmError::RateLimited { retry_after: None }
        }),
        "{events:?}"
    );
    assert!(
        events.contains(&LlmClientEvent::Retrying {
            after: secs(30),
            attempt: 2,
            reason: LlmError::RateLimited {
                retry_after: Some(secs(3_600))
            }
        }),
        "{events:?}"
    );
    assert_eq!(harness.clock.offset(), secs(32));
}

#[tokio::test]
async fn three_clock_timeouts_exhaust_the_policy() {
    let harness = ClientHarness::configured(|e| e.request_timeout = secs(10)).await;
    harness.server.enqueue([
        StubResponse::hang(),
        StubResponse::hang(),
        StubResponse::hang(),
    ]);
    let driver = harness.drive_retries();
    let client = OpenAiCompatibleClient::new(harness.endpoint.clone(), Some(API_KEY))
        .with_clock(harness.clock.clone())
        .with_observer(harness.recorder.observer());
    let task = tokio::spawn(async move { client.complete_llm(&json_request()).await });
    for attempt in 1..=3 {
        harness.server.received(attempt).await;
        assert!(
            harness.clock.wait_for_sleepers(1, SLEEPER_WAIT).await,
            "attempt {attempt} registers its timeout"
        );
        harness.clock.advance(secs(10));
    }
    let error = task.await.unwrap().unwrap_err();
    driver.abort();
    assert_eq!(error, LlmError::Timeout);
    assert_eq!(harness.server.request_count(), 3);
    let timed_out = harness
        .events()
        .iter()
        .filter(|event| matches!(event, LlmClientEvent::TimedOut { .. }))
        .count();
    assert_eq!(timed_out, 3);
    assert_eq!(harness.clock.offset(), secs(10 + 2 + 10 + 4 + 10));
}

#[tokio::test]
async fn a_mode_downgrade_does_not_consume_an_attempt() {
    let harness = ClientHarness::with_retry(RetryPolicy::with_max_attempts(2)).await;
    harness.server.enqueue([
        scripts.rejects_response_format(),
        scripts.server_error(500),
        scripts.text("{}"),
    ]);
    let driver = harness.drive_retries();
    let response = harness
        .client
        .complete_llm(&request("test", schema_format("reply")))
        .await
        .unwrap();
    driver.abort();
    assert_eq!(response.text, "{}");
    assert_eq!(harness.server.request_count(), 3);
    assert_eq!(
        format_kinds(&harness),
        [
            Some("json_schema"),
            Some("json_object"),
            Some("json_object")
        ]
    );
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::JsonObject
    );
}

fn format_kinds(harness: &ClientHarness) -> Vec<Option<&'static str>> {
    harness
        .server
        .requests()
        .iter()
        .map(|r| {
            r.chat
                .as_ref()
                .and_then(|c| c.response_format.as_ref())
                .map(|f| match f.kind.as_str() {
                    "json_schema" => "json_schema",
                    "json_object" => "json_object",
                    _ => "other",
                })
        })
        .collect()
}

// Mode fallback

#[tokio::test]
async fn a_400_naming_response_format_flips_to_json_object_and_is_remembered() {
    let harness = ClientHarness::new().await;
    harness
        .server
        .respond(scripts.server(&["stub-model"], &["json_schema"], scripts.text("{}")));
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::JsonSchema
    );
    let schema = request("test", schema_format("reply"));
    harness.client.complete_llm(&schema).await.unwrap();
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::JsonObject
    );
    assert_eq!(
        format_kinds(&harness),
        [Some("json_schema"), Some("json_object")]
    );
    assert!(harness.events().contains(&LlmClientEvent::ModeDowngraded(
        StructuredOutputMode::JsonObject
    )));
    harness.client.complete_llm(&schema).await.unwrap();
    assert_eq!(harness.server.request_count(), 3);
    assert_eq!(format_kinds(&harness)[2], Some("json_object"));
    assert_eq!(
        harness.clock.pending_sleepers(),
        0,
        "a mode downgrade is not a retry and never sleeps"
    );
}

#[tokio::test]
async fn a_second_400_falls_to_prompt_only_and_sends_no_response_format() {
    let harness = ClientHarness::new().await;
    harness.server.respond(scripts.server(
        &["stub-model"],
        &["json_schema", "json_object"],
        scripts.text("{}"),
    ));
    harness
        .client
        .complete_llm(&request("test", schema_format("reply")))
        .await
        .unwrap();
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::PromptOnly
    );
    assert_eq!(
        format_kinds(&harness),
        [Some("json_schema"), Some("json_object"), None]
    );
}

#[tokio::test]
async fn a_400_without_a_response_format_complaint_is_a_plain_http_error() {
    let harness = ClientHarness::new().await;
    harness
        .server
        .enqueue([scripts.bad_request("model `stub-model` does not exist")]);
    let error = harness
        .client
        .complete_llm(&request("test", schema_format("reply")))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        LlmError::Http {
            status: 400,
            body: "model `stub-model` does not exist".to_owned()
        }
    );
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::JsonSchema
    );
    assert_eq!(harness.server.request_count(), 1);
}

#[tokio::test]
async fn a_configured_mode_is_the_starting_point() {
    let harness =
        ClientHarness::configured(|e| e.structured_output_mode = StructuredOutputMode::PromptOnly)
            .await;
    harness.server.enqueue([scripts.text("{}")]);
    harness
        .client
        .complete_llm(&request("test", schema_format("reply")))
        .await
        .unwrap();
    assert_eq!(format_kinds(&harness), [None]);
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::PromptOnly
    );
}

#[test]
fn wire_response_format_per_mode() {
    let schema = schema_format("n");
    let kind = |format: Option<steno_llm::wire::ChatResponseFormat>| format.map(|f| f.kind);
    assert_eq!(
        kind(OpenAiCompatibleClient::response_format(
            &schema,
            StructuredOutputMode::JsonSchema
        )),
        Some("json_schema".to_owned())
    );
    assert_eq!(
        kind(OpenAiCompatibleClient::response_format(
            &schema,
            StructuredOutputMode::JsonObject
        )),
        Some("json_object".to_owned())
    );
    assert_eq!(
        OpenAiCompatibleClient::response_format(&schema, StructuredOutputMode::PromptOnly),
        None
    );
    assert_eq!(
        kind(OpenAiCompatibleClient::response_format(
            &LlmResponseFormat::JsonObject,
            StructuredOutputMode::JsonSchema
        )),
        Some("json_object".to_owned())
    );
    assert_eq!(
        OpenAiCompatibleClient::response_format(
            &LlmResponseFormat::Text,
            StructuredOutputMode::JsonSchema
        ),
        None
    );
    assert_eq!(
        StructuredOutputMode::JsonObject.downgraded(),
        Some(StructuredOutputMode::PromptOnly)
    );
    assert_eq!(StructuredOutputMode::PromptOnly.downgraded(), None);
}

#[tokio::test]
async fn a_text_request_never_downgrades() {
    let harness = ClientHarness::new().await;
    harness.server.enqueue([scripts.rejects_response_format()]);
    let error = harness
        .client
        .complete_llm(&text_request())
        .await
        .unwrap_err();
    assert_eq!(
        error,
        LlmError::Http {
            status: 400,
            body: "'response_format' of type 'json_schema' is not supported with this model"
                .to_owned()
        }
    );
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::JsonSchema
    );
    assert_eq!(harness.server.request_count(), 1);
    assert!(!harness.events().contains(&LlmClientEvent::ModeDowngraded(
        StructuredOutputMode::JsonObject
    )));
}

#[tokio::test]
async fn a_400_at_prompt_only_is_an_http_error_and_never_retried() {
    let harness =
        ClientHarness::configured(|e| e.structured_output_mode = StructuredOutputMode::PromptOnly)
            .await;
    harness.server.enqueue([scripts.rejects_response_format()]);
    let error = harness
        .client
        .complete_llm(&request("test", schema_format("reply")))
        .await
        .unwrap_err();
    assert!(
        matches!(error, LlmError::Http { status: 400, .. }),
        "{error}"
    );
    assert_eq!(harness.server.request_count(), 1);
    assert_eq!(format_kinds(&harness), [None]);
    assert_eq!(harness.clock.pending_sleepers(), 0);
}

#[tokio::test]
async fn a_json_object_request_walks_the_whole_chain_and_ends_at_prompt_only() {
    let harness = ClientHarness::new().await;
    harness
        .server
        .respond(scripts.server(&["stub-model"], &["json_object"], scripts.text("{}")));
    harness.client.complete_llm(&json_request()).await.unwrap();
    // The chain is walked mode by mode, so the JsonSchema and JsonObject
    // modes both send json_object for this request: one repeated wire
    // format before PromptOnly. Steno's builders never send JsonObject.
    assert_eq!(
        format_kinds(&harness),
        [Some("json_object"), Some("json_object"), None]
    );
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::PromptOnly
    );
    let downgrades = harness
        .events()
        .iter()
        .filter(|event| matches!(event, LlmClientEvent::ModeDowngraded(_)))
        .count();
    assert_eq!(downgrades, 2);
    harness
        .client
        .complete_llm(&request("test", schema_format("reply")))
        .await
        .unwrap();
    assert_eq!(harness.server.request_count(), 4);
    assert_eq!(format_kinds(&harness)[3], None);
    assert_eq!(harness.clock.pending_sleepers(), 0);
}

// Parameter fallback

#[tokio::test]
async fn max_tokens_and_temperature_rejections_are_resent_and_remembered() {
    let harness = ClientHarness::new().await;
    harness
        .server
        .respond(scripts.reasoning_model(scripts.text("{}")));
    let schema = request("test", schema_format("reply"));
    let response = harness.client.complete_llm(&schema).await.unwrap();
    assert_eq!(response.text, "{}");
    let wire: Vec<_> = harness
        .server
        .requests()
        .into_iter()
        .filter_map(|r| r.chat)
        .collect();
    let max: Vec<_> = wire.iter().map(|c| c.max_tokens).collect();
    assert_eq!(max, [Some(64), None, None]);
    let completion: Vec<_> = wire.iter().map(|c| c.max_completion_tokens).collect();
    assert_eq!(completion, [None, Some(64), Some(64)]);
    let temperature: Vec<_> = wire.iter().map(|c| c.temperature).collect();
    assert_eq!(temperature, [Some(0.0), Some(0.0), None]);
    assert!(
        wire.iter()
            .all(|c| c.response_format.as_ref().map(|f| f.kind.as_str()) == Some("json_schema")),
        "the mode is untouched"
    );
    let events = harness.events();
    assert!(events.contains(&LlmClientEvent::ParameterRejected("max_tokens".to_owned())));
    assert!(events.contains(&LlmClientEvent::ParameterRejected("temperature".to_owned())));
    assert_eq!(
        harness.clock.pending_sleepers(),
        0,
        "an adjustment is not a retry and never sleeps"
    );
    assert!(!events.iter().any(is_retrying));

    // Remembered: the next request goes out in the accepted spelling at once.
    harness.client.complete_llm(&text_request()).await.unwrap();
    assert_eq!(harness.server.request_count(), 4);
    let last = harness.server.requests().pop().unwrap().chat.unwrap();
    assert_eq!(last.max_tokens, None);
    assert_eq!(last.max_completion_tokens, Some(64));
    assert_eq!(last.temperature, None);
    assert_eq!(
        harness.client.resolved_mode(),
        StructuredOutputMode::JsonSchema
    );
}

#[tokio::test]
async fn the_endpoint_ceiling_is_renamed_too() {
    let harness = ClientHarness::configured(|e| e.max_output_tokens = 777).await;
    harness
        .server
        .respond(scripts.reasoning_model(scripts.text("ok")));
    let mut request = text_request();
    request.max_tokens = None;
    request.temperature = None;
    harness.client.complete_llm(&request).await.unwrap();
    let wire: Vec<_> = harness
        .server
        .requests()
        .into_iter()
        .filter_map(|r| r.chat)
        .collect();
    let max: Vec<_> = wire.iter().map(|c| c.max_tokens).collect();
    assert_eq!(max, [Some(777), None]);
    let completion: Vec<_> = wire.iter().map(|c| c.max_completion_tokens).collect();
    assert_eq!(completion, [None, Some(777)]);
    let rejections = harness
        .events()
        .iter()
        .filter(|e| **e == LlmClientEvent::ParameterRejected("max_tokens".to_owned()))
        .count();
    assert_eq!(rejections, 1);
}

#[tokio::test]
async fn a_repeated_rejection_of_the_same_parameter_is_a_plain_http_error() {
    let harness = ClientHarness::with_retry(RetryPolicy::NONE).await;
    harness.server.respond(std::sync::Arc::new(|_| {
        Some(scripts.rejects_parameter("max_tokens"))
    }));
    let error = harness
        .client
        .complete_llm(&request("test", schema_format("reply")))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        LlmError::Http {
            status: 400,
            body: "Unsupported parameter: 'max_tokens' is not supported with this model."
                .to_owned()
        }
    );
    assert_eq!(
        harness.server.request_count(),
        2,
        "renamed once, then given up"
    );
    assert_eq!(
        harness
            .server
            .requests()
            .pop()
            .unwrap()
            .chat
            .unwrap()
            .max_completion_tokens,
        Some(64)
    );
}

#[tokio::test]
async fn a_400_naming_another_parameter_is_a_plain_http_error() {
    let harness = ClientHarness::with_retry(RetryPolicy::NONE).await;
    harness
        .server
        .enqueue([scripts.rejects_parameter("messages")]);
    let error = harness
        .client
        .complete_llm(&request("test", schema_format("reply")))
        .await
        .unwrap_err();
    assert!(
        matches!(error, LlmError::Http { status: 400, .. }),
        "{error}"
    );
    assert_eq!(harness.server.request_count(), 1);
    assert!(
        !harness
            .events()
            .iter()
            .any(|e| matches!(e, LlmClientEvent::ParameterRejected(_)))
    );
    assert_eq!(
        harness.server.requests()[0]
            .chat
            .as_ref()
            .unwrap()
            .max_tokens,
        Some(64)
    );
}

#[test]
fn the_param_is_read_from_the_envelope() {
    let reply = HttpReply {
        status: 400,
        headers: HashMap::new(),
        body: b"{\"error\":{\"message\":\"Unsupported value\",\"type\":\"invalid_request_error\",\"param\":\"temperature\",\"code\":\"unsupported_value\"}}".to_vec(),
    };
    assert_eq!(
        OpenAiCompatibleClient::rejected_parameter(&reply).as_deref(),
        Some("temperature")
    );
    let plain = HttpReply {
        status: 400,
        headers: HashMap::new(),
        body: b"{\"error\":{\"message\":\"nope\"}}".to_vec(),
    };
    assert_eq!(OpenAiCompatibleClient::rejected_parameter(&plain), None);
    assert_eq!(
        OpenAiCompatibleClient::ADJUSTABLE_PARAMETERS,
        ["max_tokens", "temperature"]
    );
}

/// The client can be built from the secret store the app keeps the key in.
#[tokio::test]
async fn the_client_reads_its_key_from_the_secret_store() {
    struct OneKey;

    #[steno_core::async_trait]
    impl steno_core::SecretStore for OneKey {
        async fn secret(
            &self,
            key: &steno_core::SecretKey,
        ) -> steno_core::BoundaryResult<Option<String>> {
            Ok((key.as_str() == steno_core::SecretKey::LLM_API_KEY).then(|| API_KEY.to_owned()))
        }

        async fn set_secret(
            &self,
            _key: &steno_core::SecretKey,
            _value: Option<&str>,
        ) -> steno_core::BoundaryResult<()> {
            Ok(())
        }
    }

    let server = steno_llm::testing::StubChatServer::start().await.unwrap();
    server.enqueue([scripts.text("ok")]);
    let endpoint = LlmEndpoint::new(server.base_url().clone(), "stub-model");
    let client = OpenAiCompatibleClient::from_secret_store(endpoint, &OneKey)
        .await
        .unwrap();
    client.complete_llm(&text_request()).await.unwrap();
    assert_eq!(
        server.requests()[0].authorization(),
        Some(format!("Bearer {API_KEY}").as_str())
    );
}
