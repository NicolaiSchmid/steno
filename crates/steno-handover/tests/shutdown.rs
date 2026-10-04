//! Stopping the service closes the connections it holds, not only the
//! listener: a phone on a kept-alive connection is cut, a request sent
//! after the stop is not handled, nothing new connects, an upload stalled
//! mid-body is cut by the end of the grace, and the linger after a close
//! ends with the stop.
//! Swift: `group.shutdownGracefully()` closes the child channels.

#![allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::large_futures
)]

mod common;

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use common::{Phone, TestService, parse_response, read_until, seeded_bytes, sha256};
use steno_handover::wire;
use tokio::io::AsyncWriteExt as _;
use tokio::net::TcpStream;

const CHUNK_SIZE: i64 = 64 * 1024;
const HELLO: &[u8] = b"GET /v1/hello HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";

#[tokio::test]
async fn stop_closes_a_kept_alive_connection_and_handles_nothing_after() {
    let test = TestService::start().await;
    let port = test.port();
    let raw = test.raw_client();
    let mut stream = raw.connect().await.unwrap();
    stream.write_all(HELLO).await.unwrap();
    stream.flush().await.unwrap();
    let mut received = Vec::new();
    let closed = read_until(
        &mut stream,
        Duration::from_secs(5),
        &mut received,
        |bytes| parse_response(bytes).is_some(),
    )
    .await;
    let hello = parse_response(&received).expect("a response to the hello");
    assert_eq!(hello.status, Some(200));
    assert!(!closed, "the connection is kept alive after the hello");
    assert_eq!(hello.header("connection"), None);
    let handled = test.metrics().handled_requests;
    assert_eq!(handled, 1);

    let stopped = tokio::time::Instant::now();
    test.stop().await;
    assert!(
        stopped.elapsed() < steno_handover::server::STOP_GRACE,
        "an idle connection does not use up the grace"
    );
    let closed = read_until(&mut stream, Duration::from_secs(3), &mut Vec::new(), |_| {
        false
    })
    .await;
    assert!(closed, "the server closed the kept-alive connection");
    let metrics = test.metrics();
    assert!(metrics.closed_by_server >= 1);

    // A request on the dead connection goes nowhere, and nothing new
    // connects.
    let _ = stream.write_all(HELLO).await;
    let _ = stream.flush().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(test.metrics().handled_requests, handled);
    assert_eq!(test.metrics().request_heads, handled);
    assert!(
        TcpStream::connect(("127.0.0.1", port)).await.is_err(),
        "the listener is gone"
    );
}

#[tokio::test]
async fn stop_cuts_an_upload_stalled_mid_body_by_the_end_of_the_grace() {
    // The phone sent half a chunk and went quiet; the read timeout is far
    // off. Once `stop` returns, the connection is closed: nothing the
    // server started outlives it.
    let test = TestService::with(common::Options {
        chunk_size: CHUNK_SIZE,
        read_timeout: Duration::from_secs(60),
        ..common::Options::default()
    })
    .await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(CHUNK_SIZE as usize, 101);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    assert_eq!(phone.announce(&metadata).await.status, 201);

    let mut stream = test.raw_client().connect().await.unwrap();
    let head = format!(
        "PUT /v1/recordings/{}/chunks/0 HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: {}\r\n{}: {}\r\nContent-Length: {}\r\n\r\n",
        metadata.recording_id,
        phone.bearer(),
        wire::CHUNK_HASH_HEADER,
        STANDARD.encode(sha256(&bytes)),
        bytes.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(&bytes[..bytes.len() / 2]).await.unwrap();
    stream.flush().await.unwrap();
    let (heads, handled) = (
        test.metrics().request_heads,
        test.metrics().handled_requests,
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while test.metrics().request_heads == heads {
        assert!(tokio::time::Instant::now() < deadline, "the head arrived");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    test.stop().await;
    let closed = read_until(
        &mut stream,
        Duration::from_millis(500),
        &mut Vec::new(),
        |_| false,
    )
    .await;
    assert!(closed, "the stalled upload is closed when stop returns");
    assert_eq!(
        test.metrics().handled_requests,
        handled,
        "the chunk was not handled"
    );
}

#[tokio::test]
async fn stop_ends_the_linger_after_a_close() {
    // A request with `Connection: close` is answered and half-closed; the
    // server then reads and discards for up to CLOSE_GRACE, so the client's
    // writes land. That linger ends when the server stops: once `stop`
    // returns, the socket is gone and a write from the client is reset.
    let test = TestService::start().await;
    let mut stream = test.raw_client().connect().await.unwrap();
    stream
        .write_all(b"GET /v1/hello HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    stream.flush().await.unwrap();
    let mut received = Vec::new();
    let closed = read_until(&mut stream, Duration::from_secs(5), &mut received, |_| {
        false
    })
    .await;
    assert!(closed, "the server half-closed after the response");
    assert_eq!(parse_response(&received).and_then(|r| r.status), Some(200));

    // Well-formed records: the linger reads through TLS and would stop at
    // the first garbage byte on its own.
    let mut writes_land = async |probes: usize| {
        for _ in 0..probes {
            let written = async {
                stream.write_all(HELLO).await?;
                stream.flush().await
            };
            if written.await.is_err() {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        true
    };
    assert!(writes_land(12).await, "the socket lingers after the close");
    // A stop that waited the linger out would return about CLOSE_GRACE
    // after the close, less the probes' 300 ms. Only the stop is timed:
    // the probes before it stretch on a loaded machine.
    let stopping = tokio::time::Instant::now();
    test.stop().await;
    assert!(
        stopping.elapsed() < steno_handover::server::connection::CLOSE_GRACE / 2,
        "stop ends the linger instead of waiting it out"
    );
    assert!(
        !writes_land(20).await,
        "the lingering socket was closed by stop"
    );
}
