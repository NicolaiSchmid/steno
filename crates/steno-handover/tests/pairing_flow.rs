//! Pairing on the injected wall clock: the first use of a secret pairs,
//! the second is 403, an expired window is 403, a revoked token is 401.

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

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use common::{Phone, TestService, bearer};
use steno_handover::engine::Engine;
use steno_handover::{base64url, wire};
use uuid::Uuid;

fn device_id() -> Uuid {
    Uuid::parse_str("0BADF00D-0000-4000-8000-000000000001").unwrap()
}

#[tokio::test]
async fn first_use_pairs_and_the_second_is_forbidden() {
    let test = TestService::start().await;
    let payload = test.service.begin_pairing();
    assert_eq!(payload.mac_id, test.service.identity.mac_id());
    assert_eq!(payload.fingerprint, test.fingerprint());
    assert_eq!(payload.mac_name, "Test Mac");
    assert_eq!(
        payload.expires_at,
        test.now + chrono::Duration::seconds(300)
    );
    assert!(payload.url_string().starts_with("steno://pair/v1?mac="));

    let first = Phone::try_pair(&test, &payload.secret, device_id(), "Nicolai's iPhone").await;
    assert_eq!(first.status, 200);
    let response: wire::PairResponse = first.json();
    assert_eq!(response.mac_id, test.service.identity.mac_id());
    assert_eq!(response.mac_name, "Test Mac");
    assert_eq!(STANDARD.decode(&response.token).unwrap().len(), 32);

    let devices = test.service.paired_devices().await.unwrap();
    assert_eq!(
        devices.iter().map(|device| device.id).collect::<Vec<_>>(),
        vec![device_id()]
    );
    assert_eq!(devices[0].name, "Nicolai's iPhone");
    assert_eq!(devices[0].paired_at, test.now);

    let second = Phone::try_pair(&test, &payload.secret, device_id(), "Test iPhone").await;
    assert_eq!(second.status, 403);
    assert_eq!(
        test.metrics().handled_requests,
        1,
        "the second attempt fails at the gate"
    );

    // The token works for a bearer route, and the phone can unpair itself.
    let client = test.client();
    let unpair = client
        .request(
            "DELETE",
            "/v1/pairing",
            &[("Authorization", &bearer(&response.token))],
            None,
        )
        .await
        .unwrap();
    assert_eq!(unpair.status, 204);
    assert!(test.service.paired_devices().await.unwrap().is_empty());
    let after = client
        .request(
            "DELETE",
            "/v1/pairing",
            &[("Authorization", &bearer(&response.token))],
            None,
        )
        .await
        .unwrap();
    assert_eq!(after.status, 401, "the computer already forgot the phone");
    test.stop().await;
}

#[tokio::test]
async fn the_window_closes_after_300_seconds_on_the_injected_clock() {
    let test = TestService::start().await;
    let payload = test.service.begin_pairing();

    test.advance(Duration::from_secs(299));
    assert!(test.service.engine.pairing_is_open());
    test.advance(Duration::from_secs(2));
    assert!(!test.service.engine.pairing_is_open());

    let late = Phone::try_pair(&test, &payload.secret, device_id(), "Test iPhone").await;
    assert_eq!(late.status, 403);
    assert!(test.service.paired_devices().await.unwrap().is_empty());
    test.stop().await;
}

#[tokio::test]
async fn wrong_secrets_and_no_session_are_forbidden() {
    let test = TestService::start().await;
    let no_session = Phone::try_pair(&test, &[1u8; 32], device_id(), "x").await;
    assert_eq!(no_session.status, 403);

    let payload = test.service.begin_pairing();
    let mut wrong = payload.secret.clone();
    wrong[5] ^= 0xFF;
    assert_eq!(
        Phone::try_pair(&test, &wrong, device_id(), "x")
            .await
            .status,
        403
    );
    assert_eq!(
        Phone::try_pair(&test, &[], device_id(), "x").await.status,
        403
    );
    let client = test.client();
    let bearer_instead = client
        .json(
            "POST",
            "/v1/pair",
            &[("Authorization", &bearer(&STANDARD.encode(&payload.secret)))],
            &wire::PairRequest {
                device_id: device_id(),
                device_name: "x".to_owned(),
            },
        )
        .await;
    assert_eq!(bearer_instead.status, 403);

    // A wrong secret does not burn the session; the right one still pairs,
    // and base64url (the QR form) is accepted too.
    let url_form = client
        .json(
            "POST",
            "/v1/pair",
            &[(
                "Authorization",
                &format!("Pairing {}", base64url::encode(&payload.secret)),
            )],
            &wire::PairRequest {
                device_id: device_id(),
                device_name: "iPhone".to_owned(),
            },
        )
        .await;
    assert_eq!(url_form.status, 200);

    test.service.cancel_pairing();
    let cancelled = test.service.begin_pairing();
    test.service.cancel_pairing();
    assert_eq!(
        Phone::try_pair(&test, &cancelled.secret, device_id(), "x")
            .await
            .status,
        403
    );
    test.stop().await;
}

#[tokio::test]
async fn malformed_pair_requests_are_400_and_keep_the_window_open() {
    let test = TestService::start().await;
    let payload = test.service.begin_pairing();
    let client = test.client();

    let garbage = client
        .request(
            "POST",
            "/v1/pair",
            &[("Authorization", &common::pairing(&payload.secret))],
            Some(b"not json".to_vec()),
        )
        .await
        .unwrap();
    assert_eq!(garbage.status, 400);
    let blank = Phone::try_pair(&test, &payload.secret, device_id(), "   ").await;
    assert_eq!(blank.status, 400);
    assert!(test.service.paired_devices().await.unwrap().is_empty());

    let good = Phone::try_pair(&test, &payload.secret, device_id(), "Test iPhone").await;
    assert_eq!(good.status, 200);
    test.stop().await;
}

#[tokio::test]
async fn revoked_tokens_are_401_and_repairing_replaces_the_device() {
    let test = TestService::start().await;
    let client = test.client();

    let secret = test.service.begin_pairing().secret;
    let first = Phone::try_pair(&test, &secret, device_id(), "Test iPhone").await;
    let token = first.json::<wire::PairResponse>().token;
    let probe = client
        .get(
            &format!("/v1/recordings/{}", Uuid::new_v4()),
            &[("Authorization", &bearer(&token))],
        )
        .await;
    assert_ne!(probe.status, 401, "a fresh token passes the gate");

    test.service.revoke(device_id()).await.unwrap();
    assert!(test.service.paired_devices().await.unwrap().is_empty());
    let revoked = client
        .get(
            &format!("/v1/recordings/{}", Uuid::new_v4()),
            &[("Authorization", &bearer(&token))],
        )
        .await;
    assert_eq!(revoked.status, 401);

    let secret = test.service.begin_pairing().secret;
    let again = Phone::try_pair(&test, &secret, device_id(), "New phone").await;
    assert_eq!(again.status, 200);
    let new_token = again.json::<wire::PairResponse>().token;
    assert_ne!(new_token, token);
    let devices = test.service.paired_devices().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "New phone");
    let still_revoked = client
        .get(
            &format!("/v1/recordings/{}", Uuid::new_v4()),
            &[("Authorization", &bearer(&token))],
        )
        .await;
    assert_eq!(still_revoked.status, 401);
    test.stop().await;
}

#[test]
fn credential_parsing_is_case_insensitive_on_the_scheme() {
    assert_eq!(
        Engine::credential("Bearer", Some("Bearer abc")),
        Some("abc")
    );
    assert_eq!(
        Engine::credential("Bearer", Some("bearer abc")),
        Some("abc")
    );
    assert_eq!(Engine::credential("Bearer", Some("Pairing abc")), None);
    assert_eq!(Engine::credential("Bearer", Some("Bearer")), None);
    assert_eq!(Engine::credential("Bearer", Some("Bearer  ")), None);
    assert_eq!(Engine::credential("Bearer", None), None);
}

#[tokio::test]
async fn the_device_name_is_stored_trimmed() {
    let test = TestService::with(common::Options {
        start: false,
        ..common::Options::default()
    })
    .await;
    let _ = test.service.begin_pairing();
    let id = Uuid::new_v4();
    let response = common::engine_pair(&test, id, " \u{200B}Nicolai's iPhone\n\u{3000}").await;
    assert_eq!(response.status.as_u16(), 200);
    let stored = test.store.paired_device(id).unwrap().unwrap();
    assert_eq!(stored.name, "Nicolai's iPhone");
}

#[tokio::test]
async fn a_failed_device_save_reopens_the_window_for_the_same_secret() {
    // The secret was taken before the save; a save that fails must give
    // the window back, or the phone holds a QR code that pairs nothing.
    let test = TestService::with(common::Options {
        start: false,
        ..common::Options::default()
    })
    .await;
    let _ = test.service.begin_pairing();
    common::execute_batch(
        &test.store,
        "CREATE TEMP TRIGGER refuse_pairing BEFORE INSERT ON pairedDevice \
         BEGIN SELECT RAISE(ABORT, 'refused'); END",
    );
    let failed = common::engine_pair(&test, Uuid::new_v4(), "iPhone").await;
    assert_eq!(failed.status.as_u16(), 500);
    assert!(test.service.engine.pairing_is_open(), "the window is back");

    common::execute_batch(&test.store, "DROP TRIGGER temp.refuse_pairing");
    let id = Uuid::new_v4();
    let paired = common::engine_pair(&test, id, "iPhone").await;
    assert_eq!(paired.status.as_u16(), 200);
    assert!(!test.service.engine.pairing_is_open(), "and now spent");
    assert!(test.store.paired_device(id).unwrap().is_some());
}
