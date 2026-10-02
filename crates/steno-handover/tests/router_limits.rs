//! The two limits the plan names: bodies over chunk size plus 64 KiB are
//! answered 413 and the connection closes; unauthenticated requests are
//! answered 401 before their body is read. And the route matcher.

#![allow(
    clippy::assert_is_empty,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::large_futures,
    clippy::too_many_lines
)]

mod common;

use std::time::Duration;

use common::{TestService, bearer};
use http::Method;
use steno_handover::HandoverConfiguration;
use steno_handover::pairing::DeviceTokens;
use steno_handover::route::Route;
use steno_handover::wire;
use uuid::Uuid;

const CHUNK_SIZE: i64 = 256 * 1024;

#[tokio::test]
async fn declared_body_over_the_chunk_limit_is_413_and_closes() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let raw = test.raw_client();
    let limit = CHUNK_SIZE + HandoverConfiguration::BODY_HEADROOM;

    let exchange = raw
        .exchange(
            "PUT",
            &format!("/v1/recordings/{}/chunks/0", Uuid::new_v4()),
            &[
                ("Authorization", "Bearer nobody".to_owned()),
                ("Content-Type", "application/octet-stream".to_owned()),
                ("Content-Length", (limit + 1).to_string()),
            ],
            &vec![0x41u8; 64 * 1024],
            Duration::from_secs(3),
            Duration::from_secs(10),
        )
        .await
        .unwrap();

    assert_eq!(exchange.status, Some(413));
    assert_eq!(exchange.header("connection"), Some("close"));
    assert!(exchange.closed_by_server, "the server closes after a 413");
    assert_eq!(test.metrics().handled_requests, 0);
    assert_eq!(test.metrics().statuses, vec![413]);
    test.stop().await;
}

#[tokio::test]
async fn streamed_body_over_the_limit_is_413_and_closes() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let raw = test.raw_client();
    // No Content-Length: the counting handler must catch it as it arrives.
    // `/v1/hello` needs no auth, so the counter is the only thing in the way.
    let oversized = usize::try_from(HandoverConfiguration::JSON_BODY_LIMIT).unwrap() + 4096;
    let body = vec![0x42u8; oversized];
    let chunked = format!("{:x}\r\n", body.len())
        .into_bytes()
        .into_iter()
        .chain(body)
        .chain(b"\r\n0\r\n\r\n".iter().copied())
        .collect::<Vec<u8>>();
    let exchange = raw
        .exchange(
            "GET",
            "/v1/hello",
            &[("Transfer-Encoding", "chunked".to_owned())],
            &chunked,
            Duration::from_secs(3),
            Duration::from_secs(10),
        )
        .await
        .unwrap();

    assert_eq!(exchange.status, Some(413));
    assert!(exchange.closed_by_server);
    assert_eq!(test.metrics().handled_requests, 0);
    test.stop().await;
}

#[tokio::test]
async fn unauthenticated_put_is_answered_401_before_its_body_is_read() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let raw = test.raw_client();
    let declared = CHUNK_SIZE;
    let sent = 16 * 1024;

    // Head plus the first 16 KiB of a 256 KiB body; the rest never comes.
    let exchange = raw
        .exchange(
            "PUT",
            &format!("/v1/recordings/{}/chunks/0", Uuid::new_v4()),
            &[
                ("Authorization", bearer(&DeviceTokens::mint())),
                ("Content-Type", "application/octet-stream".to_owned()),
                ("Content-Length", declared.to_string()),
            ],
            &vec![0x43u8; sent],
            Duration::from_millis(100),
            Duration::from_secs(10),
        )
        .await
        .unwrap();

    assert_eq!(exchange.status, Some(401));
    assert_eq!(exchange.header("connection"), Some("close"));
    let problem: wire::Problem = serde_json::from_slice(&exchange.body).unwrap();
    assert!(problem.error.contains("token"));
    let metrics = test.metrics();
    assert_eq!(
        metrics.handled_requests, 0,
        "the engine never saw the request"
    );
    assert!(
        metrics.discarded_body_bytes <= sent as u64,
        "only bytes already on the wire were consumed"
    );
    assert_eq!(metrics.statuses, vec![401]);
    test.stop().await;
}

#[tokio::test]
async fn bad_pairing_secret_is_answered_403_before_its_body_is_read() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let _ = test.service.begin_pairing();
    let raw = test.raw_client();
    let declared = HandoverConfiguration::JSON_BODY_LIMIT - 1;
    let sent = 4 * 1024;

    let exchange = raw
        .exchange(
            "POST",
            "/v1/pair",
            &[
                ("Authorization", common::pairing(&[0x55u8; 32])),
                ("Content-Type", "application/json".to_owned()),
                ("Content-Length", declared.to_string()),
            ],
            &vec![0x7Bu8; sent],
            Duration::from_millis(100),
            Duration::from_secs(10),
        )
        .await
        .unwrap();

    assert_eq!(exchange.status, Some(403));
    assert_eq!(exchange.header("connection"), Some("close"));
    let problem: wire::Problem = serde_json::from_slice(&exchange.body).unwrap();
    assert!(problem.error.contains("pairing"));
    let metrics = test.metrics();
    assert_eq!(
        metrics.handled_requests, 0,
        "the engine never saw the request"
    );
    assert!(metrics.discarded_body_bytes <= sent as u64);
    assert_eq!(metrics.statuses, vec![403]);
    assert!(
        test.service.engine.pairing_is_open(),
        "a wrong secret does not burn the window"
    );
    test.stop().await;
}

#[tokio::test]
async fn missing_and_malformed_authorization_are_401() {
    let test = TestService::start().await;
    let client = test.client();

    let missing = client
        .get(&format!("/v1/recordings/{}", Uuid::new_v4()), &[])
        .await;
    assert_eq!(missing.status, 401);
    let basic = client
        .get(
            &format!("/v1/recordings/{}", Uuid::new_v4()),
            &[("Authorization", "Basic abc")],
        )
        .await;
    assert_eq!(basic.status, 401);
    let empty = client
        .get(
            &format!("/v1/recordings/{}", Uuid::new_v4()),
            &[("Authorization", "Bearer ")],
        )
        .await;
    assert_eq!(empty.status, 401);
    assert_eq!(test.metrics().handled_requests, 0);
    test.stop().await;
}

#[test]
fn route_matching_is_strict() {
    let id = Uuid::new_v4();
    let upper = id.hyphenated().to_string().to_uppercase();
    assert_eq!(
        Route::matches(&Method::GET, "/v1/hello"),
        Some(Route::Hello)
    );
    assert_eq!(
        Route::matches(&Method::GET, "/v1/hello?x=1"),
        Some(Route::Hello)
    );
    assert_eq!(Route::matches(&Method::POST, "/v1/pair"), Some(Route::Pair));
    assert_eq!(
        Route::matches(&Method::DELETE, "/v1/pairing"),
        Some(Route::Unpair)
    );
    assert_eq!(
        Route::matches(&Method::PUT, &format!("/v1/recordings/{upper}")),
        Some(Route::Announce(id))
    );
    assert_eq!(
        Route::matches(&Method::PUT, &format!("/v1/recordings/{}", id.hyphenated())),
        Some(Route::Announce(id))
    );
    assert_eq!(
        Route::matches(&Method::GET, &format!("/v1/recordings/{upper}")),
        Some(Route::Status(id))
    );
    assert_eq!(
        Route::matches(&Method::PUT, &format!("/v1/recordings/{upper}/chunks/7")),
        Some(Route::Chunk(id, 7))
    );
    assert_eq!(
        Route::matches(&Method::POST, &format!("/v1/recordings/{upper}/complete")),
        Some(Route::Complete(id))
    );
    assert_eq!(
        Route::matches(&Method::PUT, "/v1/recordings/not-a-uuid"),
        None
    );
    assert_eq!(
        Route::matches(&Method::PUT, &format!("/v1/recordings/{}", id.simple())),
        None,
        "only the hyphenated form"
    );
    assert_eq!(
        Route::matches(&Method::PUT, &format!("/v1/recordings/{upper}/chunks/-1")),
        None
    );
    assert_eq!(
        Route::matches(&Method::PUT, &format!("/v1/recordings/{upper}/chunks/07")),
        None
    );
    assert_eq!(
        Route::matches(&Method::PUT, &format!("/v1/recordings/{upper}/chunks/x")),
        None
    );
    assert_eq!(
        Route::matches(&Method::GET, &format!("/v1/recordings/{upper}/chunks/1")),
        None
    );
    assert_eq!(Route::matches(&Method::GET, "/"), None);
}
