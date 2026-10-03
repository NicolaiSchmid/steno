//! Stopping the service closes the connections it holds, not only the
//! listener: a phone on a kept-alive connection is cut, a request the
//! server stopped under is not handled, and nothing new connects.
//! Swift: `group.shutdownGracefully()` closes the child channels.

#![allow(clippy::large_futures)]

mod common;

use std::time::Duration;

use common::{TestService, parse_response, read_until};
use tokio::io::AsyncWriteExt as _;
use tokio::net::TcpStream;

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
