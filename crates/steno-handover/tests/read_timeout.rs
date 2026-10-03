//! The read timeout over a real connection: a client that stays silent is
//! cut; a request the engine is still handling is not. Swift drives the
//! handler on an embedded channel and fires the idle event by hand; here
//! the timeout is short enough to wait out.

#![allow(
    clippy::assert_is_empty,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::large_futures,
    clippy::too_many_lines
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Phone, ScriptedIntake, TestService, seeded_bytes};
use steno_handover::wire;
use uuid::Uuid;

const CHUNK_SIZE: i64 = 256 * 1024;

#[tokio::test]
async fn a_silent_connection_is_closed_on_the_read_timeout() {
    let test = TestService::with(common::Options {
        chunk_size: CHUNK_SIZE,
        read_timeout: Duration::from_millis(400),
        ..common::Options::default()
    })
    .await;
    let raw = test.raw_client();

    // Half a request line, then nothing: without a read timeout any peer on
    // the Wi-Fi could hold hundreds of such connections open for good.
    let closed = raw
        .hold_open(b"GET /v1/hel", Duration::from_secs(10))
        .await
        .unwrap();
    assert!(closed, "the server closes a connection that stays silent");
    let metrics = test.metrics();
    assert_eq!(metrics.timed_out, 1);
    assert_eq!(metrics.closed_by_server, 1);
    assert_eq!(
        metrics.request_heads, 0,
        "no request line was ever completed"
    );
    assert_eq!(metrics.handled_requests, 0);
    test.stop().await;
}

#[tokio::test]
async fn a_torn_body_is_closed_on_the_read_timeout() {
    let test = TestService::with(common::Options {
        chunk_size: CHUNK_SIZE,
        read_timeout: Duration::from_millis(400),
        ..common::Options::default()
    })
    .await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(CHUNK_SIZE as usize, 31);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    assert_eq!(phone.announce(&metadata).await.status, 201);

    // The head and 16 KiB of a 256 KiB chunk, then silence.
    // The phone's pooled connections from pairing and announcing idle out
    // meanwhile; the assertions are about the torn connection.
    let handled_before = test.metrics().handled_requests;
    let statuses_before = test.metrics().statuses.len();
    let exchange = test
        .raw_client()
        .exchange(
            "PUT",
            &format!("/v1/recordings/{}/chunks/0", metadata.recording_id),
            &[
                ("Authorization", phone.bearer()),
                ("Content-Type", "application/octet-stream".to_owned()),
                ("Content-Length", bytes.len().to_string()),
            ],
            &bytes[..16 * 1024],
            Duration::from_secs(1),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert!(
        exchange.closed_by_server,
        "the server closes the torn upload"
    );
    assert_eq!(
        exchange.status, None,
        "a torn body is closed, not answered (Swift's `errorCaught`)"
    );
    let metrics = test.metrics();
    assert!(metrics.timed_out >= 1);
    assert_eq!(
        metrics.handled_requests, handled_before,
        "the torn chunk never reached the engine"
    );
    assert_eq!(
        metrics.statuses.len(),
        statuses_before,
        "no status was written for it"
    );
    let status: wire::RecordingStatus = phone.status(metadata.recording_id).await.json();
    assert!(status.received_chunks.is_empty());
    test.stop().await;
}

#[tokio::test]
async fn the_read_timeout_does_not_cut_a_request_the_engine_is_still_handling() {
    let meeting_id = Uuid::new_v4();
    let intake = ScriptedIntake::with_delay(meeting_id, 0, Duration::from_millis(1200), false);
    let test = TestService::with(common::Options {
        chunk_size: CHUNK_SIZE,
        intake: Some(intake.clone() as Arc<dyn steno_core::HandoverIntake>),
        read_timeout: Duration::from_millis(400),
        ..common::Options::default()
    })
    .await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(CHUNK_SIZE as usize, 32);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    phone.upload_all(&metadata, &bytes).await;

    // A `complete` whose intake takes three times the timeout: the silence
    // is the computer's, so the connection that carries it is answered, not
    // cut; and it stays open afterwards for the next request. The phone's
    // own pooled connections may legitimately idle out meanwhile.
    let raw = test.raw_client();
    let completed = raw
        .exchange(
            "POST",
            &format!("/v1/recordings/{}/complete", metadata.recording_id),
            &[("Authorization", phone.bearer())],
            &[],
            Duration::from_millis(100),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert_eq!(completed.status, Some(200));
    assert!(
        !completed.closed_by_server,
        "the server kept the connection while the intake ran"
    );
    let response: wire::CompleteResponse = serde_json::from_slice(&completed.body).unwrap();
    assert_eq!(response.meeting_id, meeting_id);
    assert_eq!(intake.count(), 1);
    test.stop().await;
}
