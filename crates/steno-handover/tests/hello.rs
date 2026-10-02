//! `/v1/hello`, unknown routes, and the listener states.

#![allow(
    clippy::assert_is_empty,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::large_futures,
    clippy::too_many_lines
)]

mod common;

use common::TestService;
use steno_handover::{ListenerState, wire};

#[tokio::test]
async fn hello_answers_the_mac_id_and_protocol_without_auth() {
    let test = TestService::start().await;
    let client = test.client();

    let response = client.get("/v1/hello", &[]).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let hello: wire::Hello = response.json();
    assert_eq!(hello.mac_id, test.service.identity.mac_id());
    assert_eq!(hello.protocol, 1);
    assert_eq!(test.metrics().handled_requests, 1);
    test.stop().await;
}

#[tokio::test]
async fn unknown_routes_are_404_and_never_reach_the_engine() {
    let test = TestService::start().await;
    let client = test.client();

    for (method, path) in [
        ("GET", "/v1/nothing"),
        ("GET", "/v2/hello"),
        ("POST", "/v1/hello"),
    ] {
        let response = client.request(method, path, &[], None).await.unwrap();
        assert_eq!(response.status, 404, "{method} {path}");
    }
    assert_eq!(test.metrics().handled_requests, 0);
    test.stop().await;
}

#[tokio::test]
async fn states_follow_start_and_stop() {
    let test = TestService::with(common::Options {
        start: false,
        ..common::Options::default()
    })
    .await;
    let service = &test.service;
    let mut states = service.states();
    assert_eq!(*states.borrow_and_update(), ListenerState::Stopped);
    assert_eq!(service.state(), ListenerState::Stopped);

    service.start().await.unwrap();
    states.changed().await.unwrap();
    let listening = states.borrow_and_update().clone();
    let ListenerState::Listening { port } = listening else {
        panic!("expected a listening state with a port, got {listening:?}");
    };
    assert_ne!(port, 0);
    assert_eq!(service.state(), ListenerState::Listening { port });
    service.start().await.unwrap();
    assert_eq!(
        service.state(),
        ListenerState::Listening { port },
        "start is idempotent"
    );

    service.stop().await;
    states.changed().await.unwrap();
    assert_eq!(*states.borrow_and_update(), ListenerState::Stopped);
    assert_eq!(service.state(), ListenerState::Stopped);
}
