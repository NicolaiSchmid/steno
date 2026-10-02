//! The trust boundary: only the exact leaf fingerprint from the pairing
//! completes a handshake, with the rule the phone's
//! `PinnedTrustEvaluator.swift` applies.

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

use common::TestService;
use steno_handover::pinning::PinnedVerifier;
use steno_handover::{HandoverIdentity, PairingPayload};

#[tokio::test]
async fn the_right_fingerprint_completes_the_handshake() {
    let test = TestService::start().await;
    let response = test.client().get("/v1/hello", &[]).await;
    assert_eq!(response.status, 200);
    assert_eq!(test.metrics().request_heads, 1);
    test.stop().await;
}

#[tokio::test]
async fn one_flipped_fingerprint_bit_fails_the_handshake_before_any_request() {
    let test = TestService::start().await;
    let mut flipped = test.fingerprint();
    flipped[0] ^= 0x01;
    let client = test.client_pinning(&flipped);

    let error = client
        .request("GET", "/v1/hello", &[], None)
        .await
        .unwrap_err();
    assert!(error.is_connect() || error.is_request(), "{error}");
    assert_eq!(
        test.metrics().request_heads,
        0,
        "the server never saw a request line"
    );
    assert_eq!(test.metrics().handled_requests, 0);
    test.stop().await;
}

#[tokio::test]
async fn another_minted_certificate_is_rejected() {
    let test = TestService::start().await;
    let other = HandoverIdentity::mint("Steno on Another Mac", chrono::Utc::now()).unwrap();
    let client = test.client_pinning(&other.fingerprint());

    assert!(client.request("GET", "/v1/hello", &[], None).await.is_err());
    assert_eq!(test.metrics().request_heads, 0);
    test.stop().await;
}

#[tokio::test]
async fn the_verifier_computes_the_fingerprint_the_qr_carries() {
    // Decision 3: SHA-256 of the leaf DER on both sides. The phone hashes
    // the presented leaf; the computer hashes the DER it minted and puts it
    // in the QR as `fp`.
    let test = TestService::start().await;
    let identity = &test.service.identity;
    let payload = test.service.begin_pairing();
    let scanned = PairingPayload::parse(&payload.url_string()).unwrap();
    assert_eq!(scanned.fingerprint, identity.fingerprint().to_vec());

    let verifier = PinnedVerifier::new(&scanned.fingerprint);
    assert!(verifier.evaluate(identity.certificate_der()));
    let mut flipped = scanned.fingerprint.clone();
    flipped[17] ^= 0x40;
    assert!(!PinnedVerifier::new(&flipped).evaluate(identity.certificate_der()));
    assert!(!PinnedVerifier::new(&scanned.fingerprint[..31]).evaluate(identity.certificate_der()));
    assert!(!PinnedVerifier::new(&[]).evaluate(identity.certificate_der()));
    test.stop().await;
}

#[tokio::test]
async fn plaintext_http_to_the_listener_never_reaches_the_router() {
    // The phone builds `https://` origins only; a stray `http://` client
    // meets the TLS handshake and no request line is ever parsed.
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let test = TestService::with(common::Options {
        read_timeout: Duration::from_millis(500),
        ..common::Options::default()
    })
    .await;
    let mut plain = tokio::net::TcpStream::connect(("127.0.0.1", test.port()))
        .await
        .unwrap();
    plain
        .write_all(b"GET /v1/hello HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let mut answer = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), plain.read_to_end(&mut answer)).await;
    assert!(
        !answer.starts_with(b"HTTP/1.1"),
        "a TLS alert or nothing, never an HTTP response: {}",
        String::from_utf8_lossy(&answer)
    );
    assert_eq!(test.metrics().request_heads, 0);
    assert_eq!(test.metrics().handled_requests, 0);
    test.stop().await;
}

#[tokio::test]
async fn the_raw_client_pins_the_same_way() {
    let test = TestService::start().await;
    let raw = test.raw_client();
    let exchange = raw
        .exchange(
            "GET",
            "/v1/hello",
            &[],
            &[],
            Duration::from_millis(100),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(exchange.status, Some(200));

    let mut flipped = test.fingerprint();
    flipped[31] ^= 0x80;
    let wrong = common::RawClient {
        port: raw.port,
        fingerprint: flipped,
    };
    assert!(
        wrong
            .exchange(
                "GET",
                "/v1/hello",
                &[],
                &[],
                Duration::from_millis(100),
                Duration::from_secs(3)
            )
            .await
            .is_err()
    );
    assert_eq!(test.metrics().request_heads, 1);
    test.stop().await;
}
