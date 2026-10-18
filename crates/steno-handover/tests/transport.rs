//! The listener: TLS 1.3 to the pinned leaf only, the auth gate before any
//! body is read, the body limits, the read timeout, and a stop that closes
//! every connection.

#![allow(clippy::cast_possible_truncation, clippy::large_futures)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Options, Phone, TestService, metadata_for, read_until, seeded_bytes};
use http::Method;
use rustls::{AlertDescription, ClientConfig, SupportedProtocolVersion};
use steno_handover::configuration::HandoverConfiguration;
use steno_handover::pinning::PinnedVerifier;
use steno_handover::route::Route;
use steno_handover::{HandoverIdentity, ListenerState, PairingPayload, wire};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use uuid::Uuid;

const CHUNK_SIZE: i64 = 256 * 1024;
const HELLO: &[u8] = b"GET /v1/hello HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";

#[tokio::test]
async fn only_the_pinned_fingerprint_completes_a_handshake() {
    let test = TestService::start().await;
    let hello = test.client().send("GET", "/v1/hello", &[], None).await;
    assert_eq!(hello.status, 200, "no auth for hello");
    let hello: wire::Hello = hello.json();
    assert_eq!(hello.mac_id, test.service.identity.mac_id());
    assert_eq!(hello.protocol, wire::PROTOCOL_VERSION);

    let mut flipped = test.fingerprint();
    flipped[0] ^= 0x01;
    let other = HandoverIdentity::mint("Steno on Another Mac", chrono::Utc::now()).unwrap();
    for wrong in [flipped, other.fingerprint().to_vec()] {
        let client = common::LoopbackClient::new(test.port(), &wrong);
        assert!(client.request("GET", "/v1/hello", &[], None).await.is_err());
    }
    assert_eq!(
        test.metrics().request_heads,
        1,
        "a failed pin sends no request"
    );

    // The QR carries the fingerprint the verifier checks.
    let scanned = PairingPayload::parse(&test.service.begin_pairing().url_string()).unwrap();
    let der = test.service.identity.certificate_der();
    assert!(PinnedVerifier::new(&scanned.fingerprint).evaluate(der));
    assert!(!PinnedVerifier::new(&scanned.fingerprint[..31]).evaluate(der));
    assert!(!PinnedVerifier::new(&[]).evaluate(der));
    test.stop().await;
}

/// A handshake from a client that trusts the pinned leaf and offers only
/// `version`.
async fn handshake_offering(
    test: &TestService,
    version: &'static SupportedProtocolVersion,
) -> std::io::Result<()> {
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[version])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedVerifier::new(&test.fingerprint())))
            .with_no_client_auth();
    let tcp = TcpStream::connect(("127.0.0.1", test.port())).await?;
    let name = rustls_pki_types::ServerName::try_from("steno.local").unwrap();
    TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .map(drop)
}

#[tokio::test]
async fn tls_1_2_and_plaintext_never_reach_the_router() {
    let test = TestService::with(Options {
        read_timeout: Duration::from_millis(500),
        ..Options::default()
    })
    .await;
    handshake_offering(&test, &rustls::version::TLS13)
        .await
        .unwrap();
    let error = handshake_offering(&test, &rustls::version::TLS12)
        .await
        .unwrap_err();
    let alert = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>());
    assert!(
        matches!(
            alert,
            Some(rustls::Error::AlertReceived(
                AlertDescription::ProtocolVersion
            ))
        ),
        "{error:?}"
    );

    let mut plain = TcpStream::connect(("127.0.0.1", test.port()))
        .await
        .unwrap();
    plain.write_all(HELLO).await.unwrap();
    let mut answer = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), plain.read_to_end(&mut answer)).await;
    assert!(
        !answer.starts_with(b"HTTP/1.1"),
        "{}",
        String::from_utf8_lossy(&answer)
    );
    assert_eq!(test.metrics().request_heads, 0);
    test.stop().await;
}

#[test]
fn routes_match_strictly() {
    let id = Uuid::new_v4();
    let upper = id.hyphenated().to_string().to_uppercase();
    let cases = [
        (Method::GET, "/v1/hello?x=1".to_owned(), Some(Route::Hello)),
        (Method::POST, "/v1/pair".to_owned(), Some(Route::Pair)),
        (
            Method::DELETE,
            "/v1/pairing".to_owned(),
            Some(Route::Unpair),
        ),
        (
            Method::PUT,
            format!("/v1/recordings/{upper}"),
            Some(Route::Announce(id)),
        ),
        (
            Method::GET,
            format!("/v1/recordings/{id}"),
            Some(Route::Status(id)),
        ),
        (
            Method::PUT,
            format!("/v1/recordings/{upper}/chunks/7"),
            Some(Route::Chunk(id, 7)),
        ),
        (
            Method::POST,
            format!("/v1/recordings/{id}/complete"),
            Some(Route::Complete(id)),
        ),
        (Method::POST, "/v1/hello".to_owned(), None),
        (Method::GET, "/v2/hello".to_owned(), None),
        (Method::PUT, "/v1/recordings/not-a-uuid".to_owned(), None),
        (Method::PUT, format!("/v1/recordings/{}", id.simple()), None),
        (Method::PUT, format!("/v1/recordings/{id}/chunks/-1"), None),
        (Method::PUT, format!("/v1/recordings/{id}/chunks/07"), None),
        (Method::GET, format!("/v1/recordings/{id}/chunks/1"), None),
    ];
    for (method, path, expected) in cases {
        assert_eq!(Route::matches(&method, &path), expected, "{method} {path}");
    }
}

#[tokio::test]
async fn a_rejected_or_oversized_request_is_answered_before_its_body_and_closed() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let _ = test.service.begin_pairing();
    let chunk = format!("/v1/recordings/{}/chunks/0", Uuid::new_v4());
    let over = (CHUNK_SIZE + HandoverConfiguration::BODY_HEADROOM + 1).to_string();
    let sent = 16 * 1024;
    let cases = [
        (
            "PUT",
            chunk.as_str(),
            "Bearer nobody".to_owned(),
            CHUNK_SIZE.to_string(),
            401,
        ),
        ("PUT", &chunk, String::new(), CHUNK_SIZE.to_string(), 401),
        (
            "POST",
            "/v1/pair",
            common::pairing(&[0x55; 32]),
            "60000".to_owned(),
            403,
        ),
        ("PUT", &chunk, "Bearer nobody".to_owned(), over, 413),
    ];
    for (method, path, authorization, length, expected) in cases {
        let mut headers = vec![("Content-Length", length)];
        if !authorization.is_empty() {
            headers.push(("Authorization", authorization));
        }
        let exchange = test
            .raw_client()
            .exchange(
                method,
                path,
                &headers,
                &vec![0x43; sent],
                Duration::from_secs(1),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(exchange.status, Some(expected), "{method} {path}");
        assert_eq!(exchange.header("connection"), Some("close"), "{expected}");
    }
    let metrics = test.metrics();
    assert_eq!(metrics.handled_requests, 0, "the engine saw none of them");
    assert!(
        metrics.discarded_body_bytes <= 4 * sent as u64,
        "only bytes already sent"
    );
    assert!(
        test.service.engine.pairing_is_open(),
        "a wrong secret burns nothing"
    );
    test.stop().await;
}

#[tokio::test]
async fn silence_from_the_client_is_cut_and_a_slow_answer_is_not() {
    let test = TestService::with(Options {
        chunk_size: CHUNK_SIZE,
        delay: Duration::from_millis(1200),
        read_timeout: Duration::from_millis(400),
        ..Options::default()
    })
    .await;
    let raw = test.raw_client();
    let mut silent = raw.connect().await.unwrap();
    silent.write_all(b"GET /v1/hel").await.unwrap();
    silent.flush().await.unwrap();
    assert!(
        read_until(
            &mut silent,
            Duration::from_secs(10),
            &mut Vec::new(),
            |_| false
        )
        .await
    );
    assert_eq!(test.metrics().timed_out, 1);
    assert_eq!(test.metrics().request_heads, 0);

    // A `complete` whose intake takes three read timeouts is answered.
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(CHUNK_SIZE as usize, 32);
    let metadata = metadata_for(&bytes, CHUNK_SIZE);
    phone.upload_all(&metadata, &bytes).await;
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
    assert!(!completed.closed_by_server);
    test.stop().await;
}

#[tokio::test]
async fn stop_closes_kept_alive_and_stalled_connections_and_frees_the_port() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let mut states = test.service.states();
    let port = test.port();
    let raw = test.raw_client();
    let mut kept = raw.connect().await.unwrap();
    kept.write_all(HELLO).await.unwrap();
    let mut received = Vec::new();
    let answered = |bytes: &[u8]| common::parse_response(bytes).is_some();
    assert!(!read_until(&mut kept, Duration::from_secs(5), &mut received, answered).await);
    let mut stalled = raw.connect().await.unwrap();
    stalled
        .write_all(b"GET /v1/hello HTTP/1.1\r\nHost: x\r\nContent-Length: 1000\r\n\r\nabc")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    test.stop().await;
    assert_eq!(*states.borrow_and_update(), ListenerState::Stopped);
    for stream in [&mut kept, &mut stalled] {
        let closed = read_until(stream, Duration::from_millis(500), &mut Vec::new(), |_| {
            false
        })
        .await;
        assert!(closed, "closed by the time stop returns");
    }
    assert!(
        TcpStream::connect(("127.0.0.1", port)).await.is_err(),
        "the listener is gone"
    );
    test.stop().await;

    test.service.start().await.unwrap();
    test.service.start().await.unwrap();
    assert!(
        matches!(test.service.state(), ListenerState::Listening { .. }),
        "idempotent"
    );
    test.stop().await;
}
